//! What each `spi` command does, returning text so that tests can read it.

use std::fs;
use std::path::{Path, PathBuf};

use air_minterface::Managed;
use air_minterface::files::find_binary;
use serde::Serialize;
use serde_json::Value;

use crate::audit;
use crate::balance_figure;
use crate::bitcoind::{Node, Rpc, WalletAction};
use crate::config::{STARTER, Spi, Which, is_key_hex, url_of};
use crate::exec::{Exec, System};
use crate::interface::unpinned;
use crate::upgrade::{self, Program};
use crate::{generate, http, run, service, setup, versions, wallet};
use crate::{release, schedule};

/// The result of a command: the lines to print, and whether it succeeded.
#[derive(Debug, Default)]
pub struct Outcome {
    pub lines: Vec<String>,
    pub ok: bool,
}

impl Outcome {
    fn new() -> Self {
        Outcome {
            lines: Vec::new(),
            ok: true,
        }
    }
    fn say(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
    fn fail(&mut self, line: impl Into<String>) {
        self.ok = false;
        self.lines.push(format!("FAIL: {}", line.into()));
    }
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A mint's DID, for the SPI's wallet: `[psp] did` / `[esp] did`, else what
/// `<bin> pubkey` prints, **but only when the mint's key file already exists**
/// in the data folder spi runs it in (`pubkey` makes a key when there is none,
/// and one made in a folder the mint will not use would be a wrong DID). `None`
/// when neither is available.
pub fn did_of(spi: &Spi, exec: &dyn Exec, which: Which) -> Result<Option<String>, String> {
    if let Some(d) = &spi.mint(which).did {
        return Ok(Some(d.clone()));
    }
    let data = match which {
        Which::Psp => spi.psp_data(),
        Which::Esp => spi.data_dir(Which::Esp),
    };
    let key = data.join(spi.network.name()).join("mint_did_key.json");
    if !key.is_file() {
        return Ok(None);
    }
    let p = which.env_prefix();
    let env = vec![
        (format!("{p}_DATA_DIR"), data.display().to_string()),
        (format!("{p}_NETWORK"), spi.network.name().to_string()),
    ];
    let bin = &spi.mint(which).bin;
    let ran = exec.run(bin, &["pubkey".to_string()], &env)?;
    if !ran.ok() {
        return Err(format!("`{bin} pubkey` failed: {}", ran.last_line()));
    }
    let last = ran.stdout.trim_end().lines().last().unwrap_or("").trim();
    if is_key_hex(last) {
        Ok(Some(last.to_string()))
    } else {
        Err(format!(
            "`{bin} pubkey` printed `{last}`, which is not a DID (66 hex characters)"
        ))
    }
}

/// The data folders, then the generated config files (psp-mint, esp-mint and
/// the SPI's ecash-wallet), written again from `spi.toml`. All are checked
/// before any is written.
pub fn write_configs(spi: &Spi) -> Result<Vec<PathBuf>, String> {
    write_configs_with(spi, &System)
}

/// [`write_configs`], with the program runner given (it is asked for a mint's
/// DID).
pub fn write_configs_with(spi: &Spi, exec: &dyn Exec) -> Result<Vec<PathBuf>, String> {
    let psp = generate::render(spi, Which::Psp)?;
    let esp = generate::render(spi, Which::Esp)?;
    let psp_did = did_of(spi, exec, Which::Psp)?;
    let esp_did = did_of(spi, exec, Which::Esp)?;
    let wallet_text = generate::render_wallet(spi, psp_did.as_deref(), esp_did.as_deref())?;
    for sub in [
        "generated",
        "run",
        "logs",
        "psp-mint",
        "esp-mint",
        "air",
        "wallet",
    ] {
        let d = spi.dir.join(sub);
        fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    if let Some(dir) = spi.wallet_file().parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut written = Vec::new();
    for (path, text) in [
        (spi.generated_config(Which::Psp), psp),
        (spi.generated_config(Which::Esp), esp),
        (spi.wallet_config(), wallet_text),
    ] {
        write_text(&path, &text)?;
        written.push(path);
    }
    Ok(written)
}

/// Where `install` writes the files for the service manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceFiles {
    /// `<spi folder>/service`.
    Default,
    /// This folder.
    Into(PathBuf),
    /// Write none (`--no-service-files`).
    Skip,
}

pub struct InstallOptions {
    pub service_files: ServiceFiles,
    /// A folder whose files replace the embedded unit, property list and
    /// monitor templates (`--ops`). Optional: the binary has them all.
    pub ops_dir: Option<PathBuf>,
}

/// `spi install`: the data folders, the generated configs, a check of bitcoind
/// and its wallet, and the files that run everything as services (unless told
/// not to), with the script and the commands that put them in place.
pub fn install(spi: &Spi, opts: &InstallOptions) -> Outcome {
    let mut out = Outcome::new();
    out.say(format!("network: {}", spi.network.name()));
    match write_configs(spi) {
        Ok(files) => {
            out.say(format!("data folders ready under {}", spi.dir.display()));
            for f in files {
                out.say(format!("wrote {}", f.display()));
            }
        }
        Err(e) => {
            out.fail(e);
            return out;
        }
    }
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    let folder = match &opts.service_files {
        ServiceFiles::Skip => None,
        ServiceFiles::Default => Some(spi.dir.join("service")),
        ServiceFiles::Into(p) => Some(p.clone()),
    };
    match rpc.node(spi.network.chain()) {
        // On a fresh machine bitcoind is not there yet; the service files
        // written below are what starts it, so that is not a failure.
        Node::Unreachable(e) if folder.is_some() && spi.manage_bitcoind => out.say(format!(
            "bitcoind is not running at {} yet ({e}): the service files below start it. Run `spi install` again once it is up, to set up the wallet",
            spi.rpc_url
        )),
        Node::Unreachable(e) => {
            out.fail(format!("bitcoind is unreachable at {}: {e}", spi.rpc_url))
        }
        Node::WrongChain { found } => out.fail(format!(
            "bitcoind is on `{found}` but the config says {} (`{}`)",
            spi.network.name(),
            spi.network.chain()
        )),
        node @ (Node::Syncing(_) | Node::Synced(_)) => {
            match &node {
                Node::Syncing(c) => out.say(format!(
                    "bitcoind reachable at {}, bitcoind {}: psp-mint will not be started until it has finished syncing",
                    spi.rpc_url,
                    c.describe()
                )),
                Node::Synced(c) => out.say(format!(
                    "bitcoind reachable at {}: chain {}, {}",
                    spi.rpc_url,
                    c.chain,
                    c.describe()
                )),
                _ => {}
            }
            // A wallet can be loaded or created while the node is syncing.
            match rpc.ensure_wallet(&spi.wallet, spi.create_wallet) {
                Ok(WalletAction::AlreadyLoaded) => {
                    out.say(format!("wallet `{}` is loaded", spi.wallet))
                }
                Ok(WalletAction::Loaded) => out.say(format!("wallet `{}` was loaded", spi.wallet)),
                Ok(WalletAction::Created) => {
                    out.say(format!("wallet `{}` did not exist: created it", spi.wallet))
                }
                Err(e) => out.fail(e),
            }
        }
    }
    if spi.air.manage {
        match (&spi.air.version, find_binary(&spi.air.bin)) {
            (Some(v), Some(bin)) => match versions::record_air(spi, &bin, v) {
                Ok(()) => out.say(format!(
                    "recorded {} as air_server {v} in {} (air_server prints no version, so `spi versions` reads this)",
                    bin.display(),
                    versions::air_record_path(spi).display()
                )),
                Err(e) => out.fail(e),
            },
            (None, _) => out.say(
                "NOTE: [air] version is not set, so `spi versions` cannot say which air_server this is (it prints none)",
            ),
            (Some(_), None) => out.say(format!(
                "NOTE: `{}` (air.bin) is not on PATH, so its version is not recorded",
                spi.air.bin
            )),
        }
    }
    if let Some(folder) = folder {
        match write_service_files(spi, &folder, opts.ops_dir.as_deref()) {
            Ok(lines) => lines.into_iter().for_each(|l| out.say(l)),
            Err(e) => out.fail(format!("cannot prepare the service files: {e}")),
        }
    }
    out.say("Next: `spi setup` lists the steps to a working SPI and which are done.");
    out
}

/// The service files (see `service`) written to `folder`, and the lines that
/// tell the operator what to run. Nothing is installed and nothing is run:
/// every file is rendered before the first is written, so an error leaves the
/// folder as it was.
fn write_service_files(
    spi: &Spi,
    folder: &Path,
    ops: Option<&Path>,
) -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the spi binary: {e}"))?;
    let plan = service::plan(spi, ops, &exe)?;
    for (name, text) in &plan.files {
        write_text(&folder.join(name), text)?;
    }
    let shown = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    let names: Vec<&str> = plan.files.iter().map(|(n, _)| n.as_str()).collect();
    let mut lines = vec![
        format!(
            "service files for {} written to {}:",
            plan.system,
            shown.display()
        ),
        format!("  {}", names.join(", ")),
    ];
    for w in &plan.warnings {
        lines.push(format!("WARNING: {w}"));
    }
    for n in &plan.notes {
        lines.push(format!("NOTE: {n}"));
    }
    lines.push(format!(
        "Nothing is installed yet. Read {}, then run it as root:",
        service::SCRIPT
    ));
    lines.push(format!(
        "  sudo sh {}",
        service::sh_word(&shown.join(service::SCRIPT).display().to_string())
    ));
    lines.push(format!(
        "Put any secret (ESP_ADMIN_TOKEN) in {}/esp-mint.secret.env, then start the services:",
        spi.service.etc_dir
    ));
    for c in &plan.start {
        lines.push(format!("  {c}"));
    }
    Ok(lines)
}

/// One line of `spi status`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Component {
    pub name: String,
    pub ok: bool,
    /// `ok`, `syncing` (bitcoind), `unreachable` (bitcoind), `wrong_chain`,
    /// `not_ready` (psp-mint, because of bitcoind), or `down`.
    pub state: String,
    pub detail: String,
}

/// The service as a whole: `ok`, `degraded` (bitcoind is syncing, so PSP is not
/// ready) or `down` (anything else that is not ok).
pub fn service_state(parts: &[Component]) -> &'static str {
    if parts.iter().all(|c| c.ok) {
        "ok"
    } else if parts
        .iter()
        .all(|c| c.ok || c.state == "syncing" || c.state == "not_ready")
        && parts.iter().any(|c| c.state == "syncing")
    {
        "degraded"
    } else {
        "down"
    }
}

fn mint_status(spi: &Spi, which: Which, node: &Node) -> Component {
    let url = url_of(&spi.mint(which).bind);
    let name = which.name().to_string();
    match http::get_json(&format!("{url}/health")) {
        Err(e) => Component {
            name,
            ok: false,
            state: "down".into(),
            detail: e,
        },
        Ok((code, body)) => {
            let status = body.get("status").and_then(Value::as_str).unwrap_or("?");
            let net = body.get("network").and_then(Value::as_str);
            let mut ok = code == 200;
            let mut state = if ok { "ok" } else { "down" };
            let mut detail = format!("{url}: HTTP {code}, status {status}");
            if let Some(n) = net {
                detail.push_str(&format!(", network {n}"));
                if n != spi.network.name() {
                    ok = false;
                    state = "down";
                    detail.push_str(&format!(" (config says {})", spi.network.name()));
                }
            }
            if let Some(r) = body.get("reason").and_then(Value::as_str) {
                detail.push_str(&format!(", {r}"));
            }
            // psp-mint is only as ready as the node it scans.
            if ok && which == Which::Psp {
                match node {
                    Node::Syncing(c) => {
                        ok = false;
                        state = "not_ready";
                        detail.push_str(&format!(", NOT READY: bitcoind syncing {}%", c.percent()));
                    }
                    Node::Unreachable(_) => {
                        ok = false;
                        state = "not_ready";
                        detail.push_str(", NOT READY: bitcoind unreachable");
                    }
                    Node::WrongChain { found } => {
                        ok = false;
                        state = "not_ready";
                        detail.push_str(&format!(", NOT READY: bitcoind is on {found}"));
                    }
                    Node::Synced(_) => {}
                }
            }
            Component {
                name,
                ok,
                state: state.into(),
                detail,
            }
        }
    }
}

fn bitcoind_status(spi: &Spi, node: &Node) -> Component {
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    let name = "bitcoind".to_string();
    let (ok, state, mut detail) = match node {
        Node::Unreachable(e) => (
            false,
            "unreachable",
            format!("bitcoind unreachable at {}: {e}", spi.rpc_url),
        ),
        Node::WrongChain { found } => (
            false,
            "wrong_chain",
            format!(
                "{}: chain {found} (config says {})",
                spi.rpc_url,
                spi.network.chain()
            ),
        ),
        Node::Syncing(c) => (
            false,
            "syncing",
            format!("bitcoind {} at {}", c.describe(), spi.rpc_url),
        ),
        Node::Synced(c) => (true, "ok", format!("{}: {}", spi.rpc_url, c.describe())),
    };
    let mut ok = ok;
    let mut state = state;
    if matches!(node, Node::Syncing(_) | Node::Synced(_)) {
        match rpc.loaded_wallets() {
            Ok(w) if w.contains(&spi.wallet) => {
                detail.push_str(&format!(", wallet `{}` loaded", spi.wallet))
            }
            Ok(_) => {
                ok = false;
                if state == "ok" {
                    state = "down";
                }
                detail.push_str(&format!(", wallet `{}` NOT loaded", spi.wallet));
            }
            Err(e) => {
                ok = false;
                if state == "ok" {
                    state = "down";
                }
                detail.push_str(&format!(", wallets unreadable: {e}"));
            }
        }
    }
    Component {
        name,
        ok,
        state: state.into(),
        detail,
    }
}

fn air_status(spi: &Spi) -> Component {
    let url = run::air_health_url(spi);
    let name = "air_server".to_string();
    let managed = if spi.air.manage {
        ""
    } else {
        " (not managed by spi)"
    };
    match http::get_json(&url) {
        Ok((200, _)) => Component {
            name,
            ok: true,
            state: "ok".into(),
            detail: format!("{url}: HTTP 200{managed}"),
        },
        Ok((code, _)) => Component {
            name,
            ok: false,
            state: "down".into(),
            detail: format!("{url}: HTTP {code}{managed}"),
        },
        Err(e) => Component {
            name,
            ok: false,
            state: "down".into(),
            detail: format!("{e}{managed}"),
        },
    }
}

/// `spi status`: bitcoind, the AIR server, psp-mint and esp-mint.
pub fn status(spi: &Spi) -> Vec<Component> {
    let node = Rpc::new(&spi.rpc_url, &spi.cookie).node(spi.network.chain());
    vec![
        bitcoind_status(spi, &node),
        air_status(spi),
        mint_status(spi, Which::Psp, &node),
        mint_status(spi, Which::Esp, &node),
    ]
}

/// The setup steps (see `setup`) for the components `parts`. The SPI wallet is
/// read only once the first four steps are done.
pub fn setup_steps(spi: &Spi, parts: &[Component], exec: &dyn Exec) -> Vec<setup::Step> {
    setup::steps(parts, &|| wallet::balance(spi, exec))
}

fn lookup<'a>(t: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut parts = path.split('.');
    let mut here = t.get(parts.next()?)?;
    for part in parts {
        here = here.as_table()?.get(part)?;
    }
    Some(here)
}

/// Notes on the fees that are set in psp-mint's pass-through settings: the
/// genesis SPI buys PSP from itself, so a fee on that purchase is paid to
/// itself and only eats into what it buys.
pub fn fee_notes(spi: &Spi) -> Vec<String> {
    let mut notes = Vec::new();
    for key in ["claim.write_fee", "delivery.read_fee"] {
        if let Some(toml::Value::Integer(n)) = lookup(&spi.psp.settings, key)
            && *n != 0
        {
            notes.push(format!(
                "NOTE: psp.settings.{key} is {n}. The genesis purchase should run with every fee off: set it to 0, run `spi reload`, buy, then set it back"
            ));
        }
    }
    notes
}

/// `spi setup`: the steps to a working SPI, which are done, and what to run for
/// the first that is not. It writes the generated config files (so the wallet's
/// is there) and starts nothing.
pub fn setup(spi: &Spi, exec: &dyn Exec) -> Outcome {
    let mut out = Outcome::new();
    if let Err(e) = write_configs_with(spi, exec) {
        out.fail(e);
        return out;
    }
    let parts = status(spi);
    let steps = setup_steps(spi, &parts, exec);
    out.say(format!("network: {}", spi.network.name()));
    for s in &steps {
        let mark = if s.done { "x" } else { " " };
        let tag = if s.required { "" } else { " (optional)" };
        out.say(format!("[{mark}] {}. {}{tag}: {}", s.id, s.name, s.detail));
    }
    let ready = setup::ready(&steps);
    out.say(if ready {
        "ready: yes"
    } else {
        "ready: NO, until steps a to e are done"
    });
    let done = |id: &str| steps.iter().any(|s| s.id == id && s.done);
    if !(done("a") && done("b") && done("c") && done("d")) {
        out.say("Start the programs: `spi start` (it waits for bitcoind to sync, then starts the AIR server, psp-mint and esp-mint), or the start commands `spi install` printed for services.");
    } else if !done("e") {
        let w = wallet::prefix(spi);
        let psp_did = match did_of(spi, exec, Which::Psp) {
            Ok(Some(d)) => d,
            _ => "<the PSP's DID: set [psp] did in spi.toml>".to_string(),
        };
        out.say("Fund the SPI wallet with PSP, one of two ways.");
        out.say("  Buy PSP (the genesis SPI buys from itself, with every fee off):");
        out.say(format!("    1. {w} buy-psp --mint-key {psp_did}"));
        out.say("    2. Pay the printed payload once, as the OP_RETURN of a Bitcoin payment to the PSP mint's wallet (ecash-wallet's README shows how).");
        out.say(format!(
            "    3. {w} claim-psp   (after the payment has its confirmations)"
        ));
        out.say("  Or take in a PSP handoff from another SPI:");
        out.say(format!(
            "    {w} handoff import STRING-OR-FILE --did-key-file FILE"
        ));
        for n in fee_notes(spi) {
            out.say(n);
        }
    }
    if !done("f") {
        out.say(format!(
            "Optional, recommended if the SPI wants backups: buy FSP from another service with `{} fsp ...` (see ecash-wallet's README). spi does not track this step.",
            wallet::prefix(spi)
        ));
    }
    out
}

/// `spi collect`: take the PSP psp-mint reissued to the SPI into its wallet.
pub fn collect(spi: &Spi, exec: &dyn Exec) -> Outcome {
    let mut out = Outcome::new();
    match wallet::collect(spi, exec) {
        Ok(c) => {
            c.lines.into_iter().for_each(|l| out.say(l));
            if c.failed {
                out.ok = false;
            } else if c.taken > 0 {
                out.say(format!("took in {} file(s)", c.taken));
            }
        }
        Err(e) => {
            out.fail(e);
            return out;
        }
    }
    // With burning on, psp-mint measures its threshold against the wallet's
    // balance, which only spi can read: publish it now that what psp-mint
    // reissued is in the wallet. Without it psp-mint reissues every paid unit
    // (never burns on a guess), so a failure here is reported, not hidden.
    if spi.burn {
        match balance_figure::publish(spi, exec, &balance_figure::now_secs) {
            Ok(line) => out.say(line),
            Err(e) => out.fail(format!(
                "could not publish the SPI wallet's balance for psp-mint's burn threshold, so psp-mint reissues every paid unit until it is published: {e}"
            )),
        }
    }
    out
}

/// `spi versions`.
pub fn versions(spi: &Spi, exec: &dyn Exec) -> Vec<versions::Line> {
    versions::versions(spi, exec)
}

/// What `spi upgrade` says before it changes anything. A dry run says it too and
/// asks nothing.
pub const UPGRADE_WARNING: &str = "WARNING: spi upgrade shuts down the AIR service and every mint it runs (psp-mint, esp-mint), one program at a time, each for a brief downtime. Nothing is changed unless you confirm.";

/// Questions put to the person at the terminal. `main` asks on stdin; a test
/// answers from a closure.
pub struct Prompts<'a> {
    /// Show these lines and ask whether to go on. `true` is a yes.
    pub confirm: &'a dyn Fn(&[String]) -> bool,
    /// Show these lines (a pin and the question about it) and ask whether to
    /// reset the pin: `Some(true)` resets it, `Some(false)` keeps it, `None`
    /// means there is no terminal to ask on.
    pub pin: &'a dyn Fn(&[String]) -> Option<bool>,
}

/// What to do about pins, answered up front or asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinPolicy {
    /// Ask about each pin that is older than the newest release.
    Ask,
    /// `--keep-pins`.
    Keep,
    /// `--reset-pins`.
    Reset,
}

/// When `spi upgrade` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Now,
    /// `--at HH:MM`: the next time the local clock shows it.
    At(i8, i8),
    /// `--in DURATION`, in seconds.
    In(u64),
}

pub struct UpgradeArgs<'a> {
    /// Program names; empty for all four.
    pub names: &'a [String],
    pub dry_run: bool,
    pub when: When,
    pub pins: PinPolicy,
    pub wait_secs: u64,
}

/// How many `spi.toml.<epoch>.bak` files a pin reset keeps.
const CONFIG_BACKUPS: usize = 5;

/// What the upgrade commands work with besides the settings.
pub struct Around<'a> {
    pub exec: &'a dyn Exec,
    pub source: &'a dyn release::Source<Program>,
    pub prompts: &'a Prompts<'a>,
    /// What a program's `/health` answers.
    pub probe: &'a dyn Fn(Program) -> upgrade::Probe,
    /// The settings file (`spi.toml`), where a reset removes a pin.
    pub config: &'a Path,
    /// The clock, in the machine's zone.
    pub now: &'a jiff::Zoned,
    /// Where the scheduled job lives.
    pub job: &'a schedule::Where,
}

fn parse_programs(names: &[String]) -> Result<Vec<Program>, String> {
    let mut programs = Vec::new();
    for n in names {
        match Program::parse(n) {
            Some(p) => programs.push(p),
            None => {
                return Err(format!(
                    "`{n}` is not something spi upgrades (bitcoind, air_server, psp-mint, esp-mint)"
                ));
            }
        }
    }
    if programs.is_empty() {
        programs = Program::ALL.to_vec();
    }
    Ok(programs)
}

/// What a mint's `/health` answers, and for the others whether `status` finds
/// them ok: the probe `spi upgrade` uses.
pub fn probe_of(spi: &Spi) -> impl Fn(Program) -> upgrade::Probe + '_ {
    move |p: Program| match p {
        Program::Psp => run::probe(spi, Which::Psp),
        Program::Esp => run::probe(spi, Which::Esp),
        _ => {
            let up = status(spi).iter().any(|c| c.name == p.name() && c.ok);
            upgrade::Probe {
                up,
                ..upgrade::Probe::default()
            }
        }
    }
}

fn names_of(programs: &[Program]) -> String {
    programs
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `spi upgrade`: see `upgrade`. It warns and asks before it changes anything.
///
/// - `--dry-run`: the warning and the plan; nothing is asked or changed.
/// - Now: pins are answered (see [`PinPolicy`]), the plan is made, nothing to
///   change says so and stops; else the warning and the plan are shown and
///   confirmed, a reset removes its pin from spi.toml, and the upgrade runs.
/// - `--at` / `--in`: the warning is shown and confirmed now, the pins are
///   answered now, and the job is scheduled; the scheduled run asks nothing.
pub fn upgrade(spi: &Spi, args: &UpgradeArgs, around: &Around) -> Outcome {
    let mut out = Outcome::new();
    let programs = match parse_programs(args.names) {
        Ok(p) => p,
        Err(e) => {
            out.fail(e);
            return out;
        }
    };
    if args.dry_run && args.when != When::Now {
        out.fail("--dry-run changes nothing, so it cannot be scheduled: leave out --at and --in");
        return out;
    }
    if args.when != When::Now
        && let Err(e) = schedule::refuse_if_pending(spi, around.now)
    {
        out.fail(e);
        return out;
    }
    let held = release::pinned(spi, around.source, &programs);
    let warning = release::pin_warning(&held);
    let options = |dry_run: bool| upgrade::Options {
        dry_run,
        wait_secs: args.wait_secs,
        source: around.source,
    };

    if args.dry_run {
        out.say(UPGRADE_WARNING);
        out.lines.extend(warning);
        let done = upgrade::upgrade(spi, &programs, &options(true), around.exec, around.probe);
        out.ok = done.ok;
        out.lines.extend(done.lines);
        return out;
    }

    // The pins: asked about each that is older than a release spi knows.
    let mut reset: Vec<Program> = Vec::new();
    if !held.is_empty() {
        let mut displayed = false;
        let mut no_terminal = false;
        for h in held.iter().filter(|h| h.newest.is_some()) {
            let newest = h.newest.as_deref().unwrap_or("");
            match args.pins {
                PinPolicy::Keep => {}
                PinPolicy::Reset => reset.push(h.program),
                PinPolicy::Ask => {
                    let mut lines = Vec::new();
                    if !displayed {
                        lines.extend(warning.clone());
                    }
                    lines.push(format!(
                        "{} is pinned at {}; the newest release is {newest}. Keep the pin ({}) or reset it and install {newest}? [keep/reset]",
                        h.program.name(),
                        h.pin,
                        h.pin
                    ));
                    match (around.prompts.pin)(&lines) {
                        Some(answer) => {
                            displayed = true;
                            if answer {
                                reset.push(h.program);
                            }
                        }
                        None => no_terminal = true,
                    }
                }
            }
        }
        if !displayed {
            // Not asked (--keep-pins, --reset-pins, no terminal, or no newest
            // release known): the warning is shown all the same.
            out.lines.extend(warning);
        }
        if no_terminal {
            out.say("there is no terminal to ask on, so the pins are kept (--reset-pins installs the newest release instead)");
        }
        if args.pins == PinPolicy::Keep {
            out.say("--keep-pins: the pins are kept");
        }
        if !reset.is_empty() {
            out.say(format!(
                "the pin of {} will be reset: the newest release is installed and its `version` line is removed from {}",
                names_of(&reset),
                around.config.display()
            ));
        }
    }
    let effective = unpinned(spi, &reset);

    // What it would do.
    let mut ask = vec![UPGRADE_WARNING.to_string()];
    let names: Vec<String> = if args.when == When::Now {
        let plan = upgrade::upgrade(
            &effective,
            &programs,
            &options(true),
            around.exec,
            around.probe,
        );
        if !plan.ok || plan.changes.is_empty() {
            out.ok = plan.ok;
            out.lines.extend(plan.lines);
            return out;
        }
        ask.push(format!("This run restarts: {}.", names_of(&plan.changes)));
        ask.extend(plan.lines);
        Vec::new()
    } else {
        ask.push(format!(
            "It will upgrade: {}, without asking again.",
            names_of(&programs)
        ));
        args.names.to_vec()
    };

    // Scheduled: the question is asked now, the job runs without asking.
    if args.when != When::Now {
        let due = match args.when {
            When::At(h, m) => schedule::next_at(around.now, h, m),
            When::In(secs) => schedule::after(around.now, secs),
            When::Now => unreachable!("handled above"),
        };
        let due = match due {
            Ok(d) => d,
            Err(e) => {
                out.fail(e);
                return out;
            }
        };
        ask.push(format!(
            "It is scheduled for {}.",
            schedule::describe(&due, around.now)
        ));
        if !(around.prompts.confirm)(&ask) {
            out.ok = false;
            out.say("not confirmed: nothing was scheduled and nothing was changed");
            return out;
        }
        let pending = schedule::Pending {
            due: due.timestamp().as_second(),
            created: around.now.timestamp().as_second(),
            programs: names,
            late_secs: schedule::DEFAULT_LATE_SECS,
            reset: reset.iter().map(|p| p.name().to_string()).collect(),
        };
        match schedule::schedule(spi, around.job, around.exec, &pending, &due, around.now) {
            Ok(line) => out.say(line),
            Err(e) => out.fail(e),
        }
        return out;
    }

    if !(around.prompts.confirm)(&ask) {
        out.ok = false;
        out.say("not confirmed: nothing was changed");
        return out;
    }
    run_upgrade(
        &effective,
        &programs,
        &reset,
        args.wait_secs,
        around,
        &mut out,
    );
    out
}

/// Remove the pins in `reset` from spi.toml, then run the upgrade of `programs`
/// with `effective` (the settings without those pins). Used by a confirmed
/// upgrade and by the scheduled run.
fn run_upgrade(
    effective: &Spi,
    programs: &[Program],
    reset: &[Program],
    wait_secs: u64,
    around: &Around,
    out: &mut Outcome,
) {
    if !reset.is_empty() {
        match release::reset_pins(
            around.config,
            reset,
            around.now.timestamp().as_second(),
            CONFIG_BACKUPS,
        ) {
            Ok(lines) => lines.into_iter().for_each(|l| out.say(l)),
            Err(e) => {
                out.fail(format!("{e}: nothing was changed"));
                return;
            }
        }
    }
    let done = upgrade::upgrade(
        effective,
        programs,
        &upgrade::Options {
            dry_run: false,
            wait_secs,
            source: around.source,
        },
        around.exec,
        around.probe,
    );
    out.ok = done.ok;
    done.lines.into_iter().for_each(|l| out.say(l));
}

/// `spi upgrade run-scheduled`: what the scheduled job runs. It asks nothing.
/// Not yet due: it does nothing and the job stays. Due, and no later than the
/// allowed lateness: it upgrades. Later: it skips, says so, and changes nothing.
/// Either of the last two removes the job and its record.
pub fn run_scheduled(spi: &Spi, wait_secs: u64, around: &Around) -> Outcome {
    use schedule::Due;
    let mut out = Outcome::new();
    let now = around.now.timestamp().as_second();
    let when = |due: i64| {
        schedule::zoned(due, around.now.time_zone())
            .map(|z| schedule::describe(&z, around.now))
            .unwrap_or_else(|_| due.to_string())
    };
    let finish = |out: &mut Outcome| {
        if let Err(e) = schedule::remove(spi, around.job, around.exec) {
            out.say(format!(
                "WARNING: the scheduled job was not fully removed: {e}"
            ));
        } else {
            out.say("the scheduled job removed itself");
        }
    };
    match schedule::due_state(spi, now) {
        Err(e) => out.fail(e),
        Ok(Due::Nothing) => {
            out.say("no upgrade is scheduled: nothing to do");
            finish(&mut out);
        }
        Ok(Due::NotYet(p)) => out.say(format!("not due yet (due {}): nothing to do", when(p.due))),
        Ok(Due::TooLate(p, late)) => {
            out.say(format!(
                "skipped: the upgrade was due {} and it is now {} later, more than the {} allowed; nothing was changed",
                when(p.due),
                schedule::span_text(late),
                schedule::span_text(p.late_secs)
            ));
            finish(&mut out);
        }
        Ok(Due::Go(p)) => {
            out.say(format!("running the upgrade scheduled for {}", when(p.due)));
            match parse_programs(&p.programs) {
                Err(e) => out.fail(e),
                Ok(programs) => {
                    let reset: Vec<Program> =
                        p.reset.iter().filter_map(|n| Program::parse(n)).collect();
                    let effective = unpinned(spi, &reset);
                    run_upgrade(&effective, &programs, &reset, wait_secs, around, &mut out);
                }
            }
            finish(&mut out);
        }
    }
    out
}

/// `spi upgrade check`: the lines, and the pin warning that ends the text form.
pub struct Checked {
    pub lines: Vec<release::CheckLine>,
    pub warning: Vec<String>,
}

/// One line of `spi upgrade check --json`: the pin is `spi_toml` there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckLine {
    pub name: String,
    pub installed: String,
    pub spi_toml: String,
    pub newest: String,
    pub status: String,
    pub detail: String,
}

impl Checked {
    /// The lines as `--json` prints them.
    pub fn json(&self) -> Vec<CheckLine> {
        self.lines
            .iter()
            .map(|l| CheckLine {
                name: l.name.clone(),
                installed: l.installed.clone(),
                spi_toml: l.pin.clone(),
                newest: l.newest.clone(),
                status: l.status.clone(),
                detail: l.detail.clone(),
            })
            .collect()
    }
}

pub fn upgrade_check(spi: &Spi, exec: &dyn Exec, source: &dyn release::Source<Program>) -> Checked {
    Checked {
        lines: release::check(spi, exec, source),
        warning: release::pin_warning(&release::pinned(spi, source, &Program::ALL)),
    }
}

/// `spi upgrade status`: the pending upgrade, or that none is, then the pin
/// warning.
pub fn upgrade_status(spi: &Spi, around: &Around) -> Outcome {
    let mut out = Outcome::new();
    match schedule::status(spi, around.job, around.now) {
        Ok(lines) => lines.into_iter().for_each(|l| out.say(l)),
        Err(e) => {
            out.fail(e);
            return out;
        }
    }
    let held = release::pinned(spi, around.source, &Program::ALL);
    release::pin_warning(&held)
        .into_iter()
        .for_each(|l| out.say(l));
    out
}

/// `spi upgrade cancel`.
pub fn upgrade_cancel(spi: &Spi, around: &Around) -> Outcome {
    let mut out = Outcome::new();
    match schedule::cancel(spi, around.job, around.exec, around.now) {
        Ok(line) => out.say(line),
        Err(e) => out.fail(e),
    }
    out
}

/// `spi restart NAME`: restart psp-mint or esp-mint through the service
/// manager, so that a setting that needs a restart takes effect. It warns and
/// asks first, as `spi upgrade` does.
pub fn restart(spi: &Spi, name: &str, wait_secs: u64, around: &Around) -> Outcome {
    let mut out = Outcome::new();
    let program = match Program::parse(name) {
        Some(p) if p.is_mint() => p,
        _ => {
            out.fail(format!(
                "`{name}` is not a mint: `spi restart` restarts psp-mint or esp-mint (the other programs are restarted by `spi upgrade`)"
            ));
            return out;
        }
    };
    let ask = vec![
        format!(
            "WARNING: restarting {name} stops it and starts it again. Nothing else is stopped. Nothing is changed unless you confirm."
        ),
        upgrade::MINT_DOWNTIME.to_string(),
    ];
    if !(around.prompts.confirm)(&ask) {
        out.ok = false;
        out.say("not confirmed: nothing was restarted");
        return out;
    }
    match upgrade::restart_and_wait(spi, around.exec, program, wait_secs, around.probe) {
        Ok(()) => out.say(format!("{name}: restarted, healthy")),
        Err(e) => out.fail(format!("{name}: {e}")),
    }
    out
}

/// The command that sends `signal` to the main process of a mint's service.
pub fn signal_command(program: Program, signal: &str, mac: bool) -> (String, Vec<String>) {
    let base = program.base(mac);
    if mac {
        (
            "launchctl".into(),
            vec![
                "kill".into(),
                signal.into(),
                format!("system/org.servicepoints.{base}"),
            ],
        )
    } else {
        (
            "systemctl".into(),
            vec![
                "kill".into(),
                "-s".into(),
                signal.into(),
                "--kill-whom=main".into(),
                format!("{base}.service"),
            ],
        )
    }
}

/// `spi snapshot`: ask psp-mint and esp-mint for a live snapshot (SIGUSR1,
/// through the service manager). Each takes it while it keeps serving; its log
/// says where the copy is.
pub fn snapshot(spi: &Spi, exec: &dyn Exec) -> Outcome {
    let mut out = Outcome::new();
    for program in [Program::Psp, Program::Esp] {
        let (bin, args) = signal_command(program, "USR1", spi.mac);
        match exec.run(&bin, &args, &[]) {
            Ok(ran) if ran.ok() => out.say(format!(
                "{}: asked for a live snapshot (SIGUSR1); it keeps serving, and its log says where the copy is",
                program.name()
            )),
            Ok(ran) => out.fail(format!(
                "{}: `{}` failed: {}",
                program.name(),
                crate::exec::command_line(&bin, &args),
                ran.last_line()
            )),
            Err(e) => out.fail(format!("{}: {e}", program.name())),
        }
    }
    out
}

/// `spi publish-balance`: publish the SPI wallet's balance for psp-mint's burn
/// threshold (what the timer `spi install` writes runs). Does nothing with burn off.
pub fn publish_balance(spi: &Spi, exec: &dyn Exec) -> Outcome {
    let mut out = Outcome::new();
    if !spi.burn {
        out.say("[psp] burn is false: psp-mint reads no balance figure, so nothing is published");
        return out;
    }
    match balance_figure::publish(spi, exec, &balance_figure::now_secs) {
        Ok(line) => out.say(line),
        Err(e) => out.fail(format!(
            "could not publish the SPI wallet's balance, so psp-mint reissues every paid unit once the figure is old: {e}"
        )),
    }
    out
}

/// `spi reload`: see `run::reload`.
pub fn reload(spi: &Spi) -> Outcome {
    let mut out = Outcome::new();
    match run::reload(spi) {
        Ok(lines) => lines.into_iter().for_each(|l| out.say(l)),
        Err(e) => out.fail(e),
    }
    out
}

/// `spi audit`: the report from the two mints' `GET /audit`, with the PSP paid
/// to the SPI and the balance of the SPI's own wallet beside it. Reading the
/// wallet is allowed to fail: the audit still prints, with a warning.
pub fn audit(spi: &Spi, exec: &dyn Exec) -> Result<audit::Report, String> {
    // The wallet first and psp-mint's `/audit` second: the figure published for
    // the burn threshold needs that order (see `balance_figure`).
    let balance = wallet::balance(spi, exec);
    let balance_at = balance_figure::now_secs();
    let fetch = |which: Which| -> Result<Value, String> {
        let url = format!("{}/audit", spi.url(which));
        let (code, body) = http::get_json(&url)?;
        if code != 200 {
            return Err(format!("{url} answered HTTP {code}: {body}"));
        }
        if let Some(n) = body.get("network").and_then(Value::as_str)
            && n != spi.network.name()
        {
            return Err(format!(
                "{} is on {n} but the config says {}",
                which.name(),
                spi.network.name()
            ));
        }
        Ok(body)
    };
    let psp = fetch(Which::Psp)?;
    let esp = fetch(Which::Esp)?;
    let mut report = audit::report(&audit::figures(&psp, &esp)?)?;
    report.psp_revenue = audit::revenue(&psp)?;
    match balance
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|b| audit::wallet_report(b.psp, b.esp))
    {
        Ok(w) => report.wallet = Some(w),
        Err(e) => report
            .warnings
            .push(format!("the SPI wallet's balance was not read: {e}")),
    }
    // The same two readings are the figure psp-mint's burn threshold needs.
    if spi.burn
        && let Ok(b) = &balance
        && let Err(e) = balance_figure::publish_read(spi, b, &psp, balance_at)
    {
        report.warnings.push(format!(
            "the SPI wallet's balance was not published for psp-mint's burn threshold, so psp-mint reissues every paid unit: {e}"
        ));
    }
    Ok(report)
}

/// `spi config show`: the settings in use, where each came from, and the
/// settings passed through.
pub fn config_show(spi: &Spi, file: Option<&Path>) -> Vec<String> {
    let mut lines = vec![match file {
        Some(f) => format!("spi settings from {}, then the environment:", f.display()),
        None => "spi settings: no spi.toml, so defaults and the environment:".to_string(),
    }];
    for (key, value, from) in &spi.shown {
        lines.push(format!("  {key} = {value}   ({from})"));
    }
    lines.push(format!("  dir = {}", spi.dir.display()));
    let a = &spi.air;
    lines.push(format!(
        "air_server: {} (health {}, ws {}), the mints' air.server = {}, air.host = {}",
        if a.manage {
            "run by spi"
        } else {
            "not run by spi"
        },
        a.health_addr,
        a.ws_addr,
        a.name.as_deref().unwrap_or("(not set)"),
        a.host
    ));
    lines.push(format!(
        "  identity {}, storage {}",
        spi.air_identity(false),
        spi.air_storage(false)
    ));
    lines.push(format!(
        "ecash-wallet: {} --wallet {} --config {} (written from spi.toml by `spi install`)",
        spi.wallet_settings.bin,
        spi.wallet_file().display(),
        spi.wallet_config().display()
    ));
    match generate::wallet_table(spi, spi.psp.did.as_deref(), spi.esp.did.as_deref()) {
        Ok(t) if t.is_empty() => lines.push("  no wallet settings yet".into()),
        Ok(t) => {
            lines.push("  its settings, with where each comes from:".into());
            for (key, value) in flat(&t, "") {
                let from = if spi.wallet_settings.settings.is_empty()
                    || lookup(&spi.wallet_settings.settings, &key).is_none()
                {
                    "from [air], [psp] did, [esp] did"
                } else {
                    "[wallet.settings] in spi.toml"
                };
                lines.push(format!("    {key} = {value}   ({from})"));
            }
        }
        Err(e) => lines.push(format!("  FAIL: {e}")),
    }
    for which in [Which::Psp, Which::Esp] {
        lines.push(format!(
            "{}: {} -> {}",
            which.name(),
            spi.url(which),
            spi.generated_config(which).display()
        ));
        if spi.mint(which).settings.is_empty() {
            lines.push("  no pass-through settings".into());
        } else {
            lines.push("  passed through as set in spi.toml:".into());
            for l in toml::to_string(&spi.mint(which).settings)
                .unwrap_or_default()
                .lines()
            {
                lines.push(format!("    {l}"));
            }
        }
    }
    lines
}

/// A table's leaves as `(dotted key, value)`.
fn flat(t: &toml::Table, prefix: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, v) in t {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(sub) => out.extend(flat(sub, &key)),
            other => out.push((key, other.to_string())),
        }
    }
    out
}

/// The starter `spi.toml`, written by `install` when there is none.
pub fn write_starter(path: &Path) -> Result<(), String> {
    write_text(path, STARTER)
}
