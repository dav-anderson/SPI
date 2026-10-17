//! What each `spi` command does, returning text so that tests can read it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::audit;
use crate::bitcoind::{Node, Rpc, WalletAction};
use crate::config::{STARTER, Spi, Which, url_of};
use crate::{generate, http, service};

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

/// The data folders, then both generated config files, written again from
/// `spi.toml`. Both files are checked before either is written.
pub fn write_configs(spi: &Spi) -> Result<Vec<PathBuf>, String> {
    let psp = generate::render(spi, Which::Psp)?;
    let esp = generate::render(spi, Which::Esp)?;
    for sub in ["generated", "run", "logs", "psp-mint", "esp-mint"] {
        let d = spi.dir.join(sub);
        fs::create_dir_all(&d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    let mut written = Vec::new();
    for (which, text) in [(Which::Psp, psp), (Which::Esp, esp)] {
        let path = spi.generated_config(which);
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
    if let Some(folder) = folder {
        match write_service_files(spi, &folder, opts.ops_dir.as_deref()) {
            Ok(lines) => lines.into_iter().for_each(|l| out.say(l)),
            Err(e) => out.fail(format!("cannot prepare the service files: {e}")),
        }
    }
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
    let plan = service::plan(spi, ops)?;
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

/// `spi status`: bitcoind, psp-mint and esp-mint.
pub fn status(spi: &Spi) -> Vec<Component> {
    let node = Rpc::new(&spi.rpc_url, &spi.cookie).node(spi.network.chain());
    vec![
        bitcoind_status(spi, &node),
        mint_status(spi, Which::Psp, &node),
        mint_status(spi, Which::Esp, &node),
    ]
}

/// `spi audit`: the report from the two mints' `GET /audit`.
pub fn audit(spi: &Spi) -> Result<audit::Report, String> {
    let fetch = |which: Which| -> Result<Value, String> {
        let url = format!("{}/audit", spi.url(which));
        let (code, body) = http::get_json(&url)?;
        if code != 200 {
            return Err(format!("{url} answered HTTP {code}: {body}"));
        }
        if let Some(n) = body.get("network").and_then(Value::as_str) {
            if n != spi.network.name() {
                return Err(format!(
                    "{} is on {n} but the config says {}",
                    which.name(),
                    spi.network.name()
                ));
            }
        }
        Ok(body)
    };
    let psp = fetch(Which::Psp)?;
    let esp = fetch(Which::Esp)?;
    audit::report(&audit::figures(&psp, &esp)?)
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

/// The starter `spi.toml`, written by `install` when there is none.
pub fn write_starter(path: &Path) -> Result<(), String> {
    write_text(path, STARTER)
}
