//! The files that run psp-mint, esp-mint, the AIR server and bitcoind as services
//! on a machine that boots, generated from `spi.toml` and nothing else.
//!
//! `spi install` calls [`plan`], which returns every file as text: the mints'
//! settings files and env files, the systemd units (Linux) or launchd property
//! lists (macOS), the health monitor and its settings, and `install-system.sh`,
//! a script that puts them in place as root. Nothing here writes to the system
//! and nothing here runs as root; `commands::install` writes the plan to a
//! folder of the operator's, and the operator runs the script (see DESIGN.md,
//! "Service files", for why a script and not a `--system` flag).
//!
//! **The definitions are inside the binary.** The templates are in `ops/` in
//! this repository and are compiled in with `include_str!`, so the binary alone
//! is enough on a fresh box. A template is the unit or property list with
//! `{{name}}` where a value from `spi.toml` goes (the binary's folder, the user,
//! the data and log folders, the bitcoind datadir and arguments, ...). An
//! unknown or unclosed placeholder is an error that names it, never text left
//! in a unit. In a property list the values are XML-escaped; the keys that end
//! in `_xml` are inserted as they are. (The expander is air-minterface's
//! `template`, shared with aso.)
//!
//! **On Linux every log is held to a size cap.** Each unit runs `spi logs rotate`
//! before its program, and the logs job (`sp-logs.service`, run by
//! `sp-logs.timer`) rotates every log in place while the services run; see
//! DESIGN.md, "Service files". macOS is a development machine: no log job.
//!
//! **`--ops DIR` replaces a template and is not required.** A file in `DIR`
//! under the name `spi` writes (`systemd/psp-mint.service`,
//! `launchd/org.servicepoints.psp-mint.plist`, `sp-health-monitor.sh`, and so
//! on) is used instead of the embedded one, with the same placeholders. A file
//! that is not there falls back to the embedded one.
//!
//! **Values that go into a unit are checked.** A path, a user name or a
//! `bitcoind` argument may hold only letters, digits and `/ . _ + @ : = , -`.
//! A space or a quote would need a different escape in each of systemd, launchd
//! and sh, and a `%` or `$` means something to systemd; refusing them is simpler
//! than being sure of all three. The error names the setting.
//!
//! **Paths follow `[service]`.** The units run the binaries from `bin_dir`
//! (the script installs them there), not from `~/.cargo/bin`, because a service
//! user can not read a home folder and `ProtectHome` hides it. The service
//! bitcoind keeps its data, and so its RPC cookie, under `data_dir`, owned by
//! the same user as the mints, so psp-mint reads the cookie without any change
//! of permissions. The cookie in the service's psp-mint settings follows that
//! datadir, unless `bitcoind.cookie` was written down.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use air_minterface::air;
use air_minterface::files::find_binary;
use air_minterface::schedule;
use air_minterface::template::{expand, plain, protect_home, under_home, xml_escape};

use crate::config::{Spi, Which};
use crate::{generate, run};

const MINT_SERVICE: &str = include_str!("../ops/systemd/mint.service");
const HEALTH_SERVICE: &str = include_str!("../ops/systemd/health.service");
const BITCOIND_SERVICE: &str = include_str!("../ops/systemd/bitcoind.service");
const AIR_SERVICE: &str = include_str!("../ops/systemd/air.service");
const AIR_PLIST: &str = include_str!("../ops/launchd/air.plist");
const NGINX_HEADER: &str = include_str!("../ops/nginx-air.header");
const MINT_PLIST: &str = include_str!("../ops/launchd/mint.plist");
const HEALTH_PLIST: &str = include_str!("../ops/launchd/health.plist");
const BITCOIND_PLIST: &str = include_str!("../ops/launchd/bitcoind.plist");
const HEALTH_CONF: &str = include_str!("../ops/health.conf");
const LOGS_SERVICE: &str = include_str!("../ops/systemd/logs.service");
const LOGS_TIMER: &str = include_str!("../ops/systemd/logs.timer");
const SCRIPT_LINUX: &str = include_str!("../ops/install-system.linux.sh");
const SCRIPT_MAC: &str = include_str!("../ops/install-system.mac.sh");

/// The health monitor, as installed.
pub const HEALTH_MONITOR: &str = include_str!("../ops/sp-health-monitor.sh");

/// How often the logs job checks the logs of the running services. A log can
/// pass `[service] log_max_size_mb` by what is written in this time.
pub const LOGS_EVERY_MINUTES: u64 = 5;

/// The logs job's unit name (Linux: `sp-logs.service` and `sp-logs.timer`) and
/// its launchd label.
pub const LOGS_UNIT: &str = "sp-logs";

/// The name of the script that puts the files in place.
pub const SCRIPT: &str = "install-system.sh";

/// What `plan` returns.
#[derive(Debug)]
pub struct Plan {
    /// The system the files are for: `Linux (systemd)` or `macOS (launchd)`.
    pub system: &'static str,
    /// File name and text, in the order they are written. The script is last.
    pub files: Vec<(String, String)>,
    /// What may be wrong, and the operator should know before running the script.
    pub warnings: Vec<String>,
    /// What the operator should know about how the files work.
    pub notes: Vec<String>,
    /// The commands that start the services, after the script has run.
    pub start: Vec<String>,
    /// The folder the unit files are installed into.
    pub unit_dir: &'static str,
}

/// `value` as one word for sh: as it is when it is plain, else in single quotes.
pub fn sh_word(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._+@:=,-".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Where bitcoind keeps its data when it runs as a service.
pub fn bitcoind_datadir(spi: &Spi) -> String {
    match &spi.bitcoind_datadir {
        Some(d) => d.clone(),
        None => format!("{}/bitcoind", spi.service.data_dir),
    }
}

fn url_port(url: &str) -> Option<u16> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split('/').next()?;
    host.rsplit_once(':')?.1.parse().ok()
}

/// bitcoind's command line as a service: in the foreground, with the chain, its
/// data folder, a cookie the service group can read, the RPC port when it is
/// not the chain's own, and `bitcoind.args`.
pub fn bitcoind_args(spi: &Spi) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    if let Some(flag) = run::chain_flag(spi.network) {
        args.push(flag.to_string());
    }
    args.push(format!("-datadir={}", bitcoind_datadir(spi)));
    // The cookie is readable by the service group, so that a person who is in
    // it can run `spi install` and `spi status` against this node.
    args.push("-rpccookieperms=group".to_string());
    // The whole log goes to the console (the unit's `bitcoind.log`), which the
    // log cap covers; bitcoind trims `debug.log` only when it starts.
    args.push("-nodebuglogfile".to_string());
    args.push("-printtoconsole".to_string());
    if let Some(port) = url_port(&spi.rpc_url)
        && port != spi.network.rpc_port()
    {
        args.push(format!("-rpcport={port}"));
    }
    for a in &spi.bitcoind_args {
        if a == "-daemon" || a.starts_with("-daemon=") {
            return Err(format!(
                "bitcoind.args has `{a}`: a service runs bitcoind in the foreground, so it must not daemonize"
            ));
        }
        args.push(a.clone());
    }
    Ok(args)
}

/// The settings as the service runs them: the same, except that the cookie is
/// where the service's own bitcoind writes it, unless `bitcoind.cookie` was
/// written down or bitcoind is not managed. `notes` gets what the operator needs
/// to know about that.
pub fn service_view(spi: &Spi, warnings: &mut Vec<String>, notes: &mut Vec<String>) -> Spi {
    let mut view = spi.clone();
    // psp-mint reads the wallet's balance figure from the folder the script
    // makes for `spi` to write in, unless the setting names another.
    if spi.burn && spi.burn_balance_file.is_none() {
        view.burn_balance_file = Some(format!(
            "{}/spi-share/{}",
            spi.service.data_dir,
            crate::balance_figure::FILE
        ));
    }
    let mut derived = false;
    if spi.manage_bitcoind {
        let mut c = PathBuf::from(bitcoind_datadir(spi));
        let sub = spi.network.subdir();
        if !sub.is_empty() {
            c.push(sub);
        }
        c.push(".cookie");
        if !spi.cookie_set {
            derived = true;
            notes.push(format!(
                "psp-mint reads the service bitcoind's cookie at {}. spi itself, run as you, reads {}: to run `spi install` and `spi status` against the service bitcoind, add yourself to the group `{}` (it can read the cookie, which is the node's RPC password: add only people who may use the node's wallet) and set `cookie = \"{}\"` under [bitcoind] in spi.toml",
                c.display(),
                spi.cookie.display(),
                spi.service.group,
                c.display()
            ));
            view.cookie = c;
        } else if spi.cookie != c {
            warnings.push(format!(
                "bitcoind.cookie is {}, so psp-mint reads it there; the service bitcoind writes its cookie at {}",
                spi.cookie.display(),
                c.display()
            ));
        }
    }
    let cookie = view.cookie.display().to_string();
    if !derived && under_home(&cookie) {
        warnings.push(format!(
            "the RPC cookie is {cookie}, in a home folder: the service user ({}) must be able to read it, and the psp-mint unit then has ProtectHome=read-only. Point bitcoind.cookie at a readable copy, or let spi manage bitcoind",
            spi.service.user
        ));
    }
    view
}

/// What may be wrong with the AIR settings as the mints and the wallet will use
/// them, which is not an error of the settings alone.
pub fn air_warnings(spi: &Spi) -> Vec<String> {
    let mut out = Vec::new();
    if spi.air.name.is_none() {
        out.push(
            "[air] name is not set, so the generated psp-mint and esp-mint files have no air.server and air.host, and neither mint can start (psp-mint refuses to start without them). Set [air] name to the AIR server's public key (66 hex characters)".into(),
        );
    }
    let local = ["127.0.0.1", "localhost", "::1"].contains(&spi.air.host.as_str());
    let port = spi.air.ws_addr.rsplit(':').next().unwrap_or("");
    if spi.air.manage && local && port != "5702" {
        out.push(format!(
            "the mints dial port 5702 of air.host ({}), but air.ws_addr is {}: they will not reach the AIR server unless something forwards 5702 to it",
            spi.air.host, spi.air.ws_addr
        ));
    }
    out
}

fn env_file(spi: &Spi, which: Which) -> String {
    let (name, p, svc) = (which.name(), which.env_prefix(), &spi.service);
    format!(
        "# Written by `spi install` from spi.toml. Don't edit: change spi.toml and run\n\
         # `spi install` again, which writes this file again. Secrets go in\n\
         # {name}.secret.env, which spi never overwrites.\n\
         # KEY=VALUE, one a line, no export.\n\
         HOME={data}/{name}\n\
         {p}_DATA_DIR={data}/{name}\n\
         {p}_CONFIG={etc}/{name}.toml\n\
         {p}_NETWORK={network}\n",
        data = svc.data_dir,
        etc = svc.etc_dir,
        network = spi.network.name(),
    )
}

fn secret_file(spi: &Spi, which: Which) -> String {
    let (name, svc) = (which.name(), &spi.service);
    let mut text = format!(
        "# {etc}/{name}.secret.env  (root:{group}, mode 0640)\n\
         # Written once by `spi install`; after that it is yours, and spi never\n\
         # overwrites it. KEY=VALUE, one a line, no export; a value with a space goes\n\
         # in double quotes. Read after {name}.env, so it wins. Put secrets here, not\n\
         # in spi.toml.\n",
        etc = svc.etc_dir,
        group = svc.group,
    );
    if which == Which::Esp {
        text.push_str(
            "# Makes the operator routes (payouts, pay, decline) need a bearer token.\n\
             # ESP_ADMIN_TOKEN=\n",
        );
    }
    text
}

/// A template from `ops` under `rel` if there is one, else the embedded text.
fn template(embedded: &str, ops: Option<&Path>, rel: &str) -> Result<String, String> {
    let Some(dir) = ops else {
        return Ok(embedded.to_string());
    };
    let path = dir.join(rel);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(embedded.to_string()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// The values every unit has.
fn common_vars(spi: &Spi, protect: &str) -> Vec<(&'static str, String)> {
    let svc = &spi.service;
    vec![
        ("user", svc.user.clone()),
        ("group", svc.group.clone()),
        ("bin_dir", svc.bin_dir.clone()),
        ("etc_dir", svc.etc_dir.clone()),
        ("data_dir", svc.data_dir.clone()),
        ("log_dir", svc.log_dir.clone()),
        ("network", spi.network.name().to_string()),
        ("protect_home", protect.to_string()),
        ("spi_bin", format!("{}/spi", svc.bin_dir)),
        ("log_max_mb", svc.log_max_size_mb.to_string()),
        ("log_keep", svc.log_keep.to_string()),
        (
            "rotate_cmd",
            format!(
                "{}/spi logs rotate --max-mb {} --keep {}",
                svc.bin_dir, svc.log_max_size_mb, svc.log_keep
            ),
        ),
    ]
}

/// A `put` or `keep` line for the script.
fn install_line(
    verb: &str,
    mode: &str,
    owner: &str,
    group: &str,
    source: &str,
    dest: &str,
) -> String {
    format!("{verb} {mode} {owner} {group} {source} {dest}")
}

/// The health monitor's files for one service: its settings file (written once,
/// then yours) and its unit or property list. `base` is the service's unit name
/// (`psp-mint`, `sp-air-server`), `url` what the monitor asks.
fn health_entry(
    spi: &Spi,
    ops: Option<&Path>,
    base: &str,
    url: &str,
    files: &mut Vec<(String, String)>,
    installs: &mut Vec<String>,
) -> Result<(), String> {
    let svc = &spi.service;
    let mac = spi.mac;
    let (rel_dir, root_group) = if mac {
        ("launchd", "wheel")
    } else {
        ("systemd", "root")
    };
    let health_paths = [
        svc.bin_dir.as_str(),
        svc.etc_dir.as_str(),
        svc.log_dir.as_str(),
    ];
    let mut vars = common_vars(spi, protect_home(&health_paths));
    vars.push(("name", base.to_string()));
    vars.push(("health_url", url.to_string()));
    vars.push((
        "restart_cmd",
        if mac {
            format!("launchctl kickstart -k system/org.servicepoints.{base}")
        } else {
            format!("systemctl restart {base}.service")
        },
    ));
    let conf = format!("{base}-health.conf");
    files.push((conf.clone(), expand(&conf, HEALTH_CONF, &vars, false)?));
    installs.push(install_line(
        "keep",
        "0644",
        "root",
        root_group,
        &format!("\"$here/{conf}\""),
        &format!("\"$etc_dir/{conf}\""),
    ));
    let file = if mac {
        format!("org.servicepoints.{base}-health.plist")
    } else {
        format!("{base}-health.service")
    };
    let (embedded, what) = if mac {
        (HEALTH_PLIST, "the health monitor property list")
    } else {
        (HEALTH_SERVICE, "the health monitor unit")
    };
    let text = template(embedded, ops, &format!("{rel_dir}/{file}"))?;
    files.push((
        file.clone(),
        expand(&format!("{what} {file}"), &text, &vars, mac)?,
    ));
    installs.push(install_line(
        "put",
        "0644",
        "root",
        root_group,
        &format!("\"$here/{file}\""),
        &format!("\"$unit_dir/{file}\""),
    ));
    Ok(())
}

/// The logs job's files (Linux only: a service and a timer). `logs` are the
/// files it keeps under the cap.
fn logs_entry(
    spi: &Spi,
    ops: Option<&Path>,
    logs: &[String],
    files: &mut Vec<(String, String)>,
    installs: &mut Vec<String>,
) -> Result<(), String> {
    let svc = &spi.service;
    for l in logs {
        plain("a log file", l)?;
    }
    let mut vars = common_vars(spi, protect_home(&[svc.log_dir.as_str()]));
    vars.push(("minutes", LOGS_EVERY_MINUTES.to_string()));
    // `plain` paths are single shell words as they are.
    vars.push(("logs", logs.join(" ")));
    let parts = [
        (
            format!("{LOGS_UNIT}.service"),
            LOGS_SERVICE,
            "the logs unit",
        ),
        (format!("{LOGS_UNIT}.timer"), LOGS_TIMER, "the logs timer"),
    ];
    installs.push("# The job that keeps the service logs under their size cap.".into());
    for (file, embedded, what) in parts {
        let text = template(embedded, ops, &format!("systemd/{file}"))?;
        files.push((
            file.clone(),
            expand(&format!("{what} {file}"), &text, &vars, false)?,
        ));
        installs.push(install_line(
            "put",
            "0644",
            "root",
            "root",
            &format!("\"$here/{file}\""),
            &format!("\"$unit_dir/{file}\""),
        ));
    }
    Ok(())
}

/// Every file for the service: see the module documentation.
pub fn plan(spi: &Spi, ops: Option<&Path>, exe: &Path) -> Result<Plan, String> {
    if let Some(dir) = ops
        && !dir.is_dir()
    {
        return Err(format!("--ops {} is not a folder", dir.display()));
    }
    let svc = &spi.service;
    let mac = spi.mac;
    let manage = spi.manage_bitcoind;
    let mut warnings = Vec::new();

    for (what, value) in [
        ("service.user", &svc.user),
        ("service.group", &svc.group),
        ("service.bin_dir", &svc.bin_dir),
        ("service.etc_dir", &svc.etc_dir),
        ("service.data_dir", &svc.data_dir),
        ("service.log_dir", &svc.log_dir),
    ] {
        plain(what, value)?;
    }
    let datadir = bitcoind_datadir(spi);
    let args = if manage {
        plain("the bitcoind data folder (bitcoind.datadir)", &datadir)?;
        let args = bitcoind_args(spi)?;
        for a in &args {
            plain("a bitcoind argument", a)?;
        }
        if under_home(&datadir) {
            warnings.push(format!(
                "bitcoind.datadir is {datadir}, in a home folder: the service user ({}) must own it, and the bitcoind unit then has ProtectHome=read-only",
                svc.user
            ));
        }
        args
    } else {
        Vec::new()
    };
    let mut notes = Vec::new();
    let view = service_view(spi, &mut warnings, &mut notes);
    warnings.extend(air_warnings(spi));

    let (unit_dir, rel_dir) = if mac {
        ("/Library/LaunchDaemons", "launchd")
    } else {
        ("/etc/systemd/system", "systemd")
    };
    let unit_name = |base: &str| -> String {
        if mac {
            format!("org.servicepoints.{base}.plist")
        } else {
            format!("{base}.service")
        }
    };
    let root_group = if mac { "wheel" } else { "root" };
    let bitcoind_base = if mac { "bitcoind" } else { "sp-bitcoind" };

    let mut files: Vec<(String, String)> = Vec::new();
    // Lines of the script, and the commands that start the services.
    let mut installs: Vec<String> = Vec::new();
    let mut start_units: Vec<String> = Vec::new();

    // The binaries.
    let exe_text = exe.display().to_string();
    let mut binaries = vec![
        ("psp-mint", spi.psp.bin.as_str()),
        ("esp-mint", spi.esp.bin.as_str()),
    ];
    if !mac {
        // The Linux units run `spi logs rotate`, so a copy of spi goes beside them.
        binaries.push(("spi", exe_text.as_str()));
    }
    if manage {
        binaries.push(("bitcoind", spi.bitcoind_bin.as_str()));
    }
    if spi.air.manage {
        binaries.push(("air_server", spi.air.bin.as_str()));
    }
    installs.push("# The binaries.".into());
    for (name, spec) in binaries {
        let dest = format!("\"$bin_dir/{name}\"");
        match find_binary(spec) {
            Some(src) => installs.push(install_line(
                "put",
                "0755",
                "root",
                root_group,
                &sh_word(&src.display().to_string()),
                &dest,
            )),
            None => {
                warnings.push(format!(
                    "`{spec}` is not on PATH or not a file, so the script cannot install {name}: put it at {}/{name} (README, \"Getting the binaries\")",
                    svc.bin_dir
                ));
                installs.push(format!("need {dest} {name}"));
            }
        }
    }

    // The mints' settings, env files and units.
    for which in [Which::Psp, Which::Esp] {
        let name = which.name();
        files.push((format!("{name}.toml"), generate::render(&view, which)?));
        files.push((format!("{name}.env"), env_file(spi, which)));
        files.push((format!("{name}.secret.env"), secret_file(spi, which)));
        installs.push(format!("# {name}."));
        for ext in ["toml", "env"] {
            installs.push(install_line(
                "put",
                "0640",
                "root",
                "\"$group\"",
                &format!("\"$here/{name}.{ext}\""),
                &format!("\"$etc_dir/{name}.{ext}\""),
            ));
        }
        installs.push(install_line(
            "keep",
            "0640",
            "root",
            "\"$group\"",
            &format!("\"$here/{name}.secret.env\""),
            &format!("\"$etc_dir/{name}.secret.env\""),
        ));

        let cookie = view.cookie.display().to_string();
        let mut paths = vec![
            svc.bin_dir.as_str(),
            svc.etc_dir.as_str(),
            svc.data_dir.as_str(),
            svc.log_dir.as_str(),
        ];
        if which == Which::Psp {
            paths.push(&cookie);
        }
        let mut vars = common_vars(spi, protect_home(&paths));
        let (description, wait) = match which {
            Which::Psp => (
                "the PSP mint (Bitcoin purchases to PSP notes)",
                match (manage && !mac, spi.air.manage && !mac) {
                    (true, true) => {
                        "network-online.target sp-bitcoind.service sp-air-server.service"
                    }
                    (true, false) => "network-online.target sp-bitcoind.service",
                    (false, true) => "network-online.target sp-air-server.service",
                    (false, false) => "network-online.target",
                },
            ),
            Which::Esp => (
                "the ESP mint (PSP to ESP, transfer, redemption)",
                "network-online.target psp-mint.service",
            ),
        };
        vars.push(("name", name.to_string()));
        vars.push(("description", description.to_string()));
        vars.push(("after", wait.to_string()));
        vars.push(("wants", wait.to_string()));
        let file = unit_name(name);
        let (embedded, what) = if mac {
            (MINT_PLIST, "the mint property list")
        } else {
            (MINT_SERVICE, "the mint unit")
        };
        let text = template(embedded, ops, &format!("{rel_dir}/{file}"))?;
        files.push((
            file.clone(),
            expand(&format!("{what} {file}"), &text, &vars, mac)?,
        ));
        installs.push(install_line(
            "put",
            "0644",
            "root",
            root_group,
            &format!("\"$here/{file}\""),
            &format!("\"$unit_dir/{file}\""),
        ));

        if svc.health {
            health_entry(
                spi,
                ops,
                name,
                &format!("{}/health", spi.url(which)),
                &mut files,
                &mut installs,
            )?;
        }
    }

    if svc.health {
        files.push((
            "sp-health-monitor.sh".into(),
            template(HEALTH_MONITOR, ops, "sp-health-monitor.sh")?,
        ));
        installs.push("# The health monitors.".into());
        installs.push(install_line(
            "put",
            "0755",
            "root",
            root_group,
            "\"$here/sp-health-monitor.sh\"",
            "\"$bin_dir/sp-health-monitor.sh\"",
        ));
    }

    // bitcoind.
    let mut folders = vec![
        "\"$data_dir/psp-mint\"".to_string(),
        "\"$data_dir/esp-mint\"".to_string(),
    ];
    if manage {
        let own = format!("{}/bitcoind", svc.data_dir);
        folders.push(if datadir == own {
            "\"$data_dir/bitcoind\"".to_string()
        } else {
            sh_word(&datadir)
        });
        let paths = [svc.bin_dir.as_str(), datadir.as_str(), svc.log_dir.as_str()];
        let mut vars = common_vars(spi, protect_home(&paths));
        vars.push(("bitcoind_bin", format!("{}/bitcoind", svc.bin_dir)));
        vars.push(("bitcoind_args", args.join(" ")));
        vars.push((
            "bitcoind_args_xml",
            args.iter()
                .map(|a| format!("        <string>{}</string>", xml_escape(a)))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        vars.push(("bitcoind_datadir", datadir.clone()));
        let file = unit_name(bitcoind_base);
        let (embedded, what) = if mac {
            (BITCOIND_PLIST, "the bitcoind property list")
        } else {
            (BITCOIND_SERVICE, "the bitcoind unit")
        };
        let text = template(embedded, ops, &format!("{rel_dir}/{file}"))?;
        files.push((
            file.clone(),
            expand(&format!("{what} {file}"), &text, &vars, mac)?,
        ));
        installs.push("# bitcoind.".into());
        installs.push(install_line(
            "put",
            "0644",
            "root",
            root_group,
            &format!("\"$here/{file}\""),
            &format!("\"$unit_dir/{file}\""),
        ));
    }

    // The AIR server.
    let air_base = if mac { "air-server" } else { "sp-air-server" };
    let mut air_units: Vec<String> = Vec::new();
    if spi.air.manage {
        let env = generate::air_env(spi, true);
        for (key, value) in &env {
            plain(&format!("the AIR server's {key}"), value)?;
        }
        let storage = spi.air_storage(true);
        let identity = spi.air_identity(true);
        let air_data = Path::new(&storage)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("{}/air-server", svc.data_dir));
        plain("the AIR server's data folder (air.storage)", &air_data)?;
        folders.push(if air_data == format!("{}/air-server", svc.data_dir) {
            "\"$data_dir/air-server\"".to_string()
        } else {
            sh_word(&air_data)
        });
        let env_text = format!(
            "# Written by `spi install` from spi.toml. Don't edit: change spi.toml and run\n\
             # `spi install` again, which writes this file again.\n\
             # KEY=VALUE, one a line, no export.\n{}",
            env.iter()
                .map(|(k, v)| format!("{k}={v}\n"))
                .collect::<String>()
        );
        files.push(("air-server.env".into(), env_text));
        installs.push("# The AIR server.".into());
        installs.push(install_line(
            "put",
            "0640",
            "root",
            "\"$group\"",
            "\"$here/air-server.env\"",
            "\"$etc_dir/air-server.env\"",
        ));
        installs.push(format!(
            "needfile {} {}",
            sh_word(&identity),
            sh_word(
                "the AIR server's identity (AIR_SECRET_FILE): you make it, and keep an offline copy; spi does neither"
            )
        ));
        let paths = [
            svc.bin_dir.as_str(),
            air_data.as_str(),
            svc.log_dir.as_str(),
            identity.as_str(),
        ];
        let mut vars = common_vars(spi, protect_home(&paths));
        vars.push(("name", air_base.to_string()));
        vars.push(("air_data_dir", air_data.clone()));
        let file = unit_name(air_base);
        let (embedded, what) = if mac {
            (AIR_PLIST, "the AIR server property list")
        } else {
            (AIR_SERVICE, "the AIR server unit")
        };
        let text = template(embedded, ops, &format!("{rel_dir}/{file}"))?;
        files.push((
            file.clone(),
            expand(&format!("{what} {file}"), &text, &vars, mac)?,
        ));
        installs.push(install_line(
            "put",
            "0644",
            "root",
            root_group,
            &format!("\"$here/{file}\""),
            &format!("\"$unit_dir/{file}\""),
        ));
        air_units.push(air_base.to_string());
        if svc.health {
            health_entry(
                spi,
                ops,
                air_base,
                &format!("http://{}/", spi.air.health_addr),
                &mut files,
                &mut installs,
            )?;
            air_units.push(format!("{air_base}-health"));
        }
        notes.push(format!(
            "The AIR server's identity is {identity}: you make it and you keep an offline copy of it, spi does neither. Losing it changes the server's name, and then every mint and wallet needs the new one ([air] name)"
        ));
        if let (Some(name), Some(cert), Some(key)) = (
            &spi.air.public_name,
            &spi.air.public_cert,
            &spi.air.public_key,
        ) {
            let vars = vec![
                ("public_name", name.clone()),
                ("public_cert", cert.clone()),
                ("public_key", key.clone()),
                ("ws_addr", spi.air.ws_addr.clone()),
            ];
            files.push((
                "nginx-air-server.conf".into(),
                expand(
                    "the nginx file",
                    &air::nginx_template(NGINX_HEADER),
                    &vars,
                    false,
                )?,
            ));
            notes.push(
                "nginx-air-server.conf is for nginx: spi does not install it. Put it where your nginx reads its sites, check it with `nginx -t` and reload nginx".into(),
            );
        }
    }

    // How the services are started, in order. bitcoind comes first and alone,
    // and the wallet is set up once it answers, so psp-mint finds it. Then the
    // AIR server, which the mints dial, then the mints.
    let mut mint_units: Vec<String> = Vec::new();
    for which in [Which::Psp, Which::Esp] {
        mint_units.push(which.name().to_string());
    }
    if svc.health {
        for which in [Which::Psp, Which::Esp] {
            mint_units.push(format!("{}-health", which.name()));
        }
    }
    let start_lines = |bases: &[String]| -> Vec<String> {
        if mac {
            bases
                .iter()
                .map(|base| {
                    format!(
                        "sudo launchctl bootstrap system {unit_dir}/org.servicepoints.{base}.plist"
                    )
                })
                .collect()
        } else {
            let list: Vec<String> = bases.iter().map(|b| format!("{b}.service")).collect();
            vec![format!("sudo systemctl enable --now {}", list.join(" "))]
        }
    };
    if manage {
        start_units.extend(start_lines(&[bitcoind_base.to_string()]));
        start_units
            .push("spi install --no-service-files   (again, once bitcoind answers: it sets up the wallet)".into());
    }
    if !air_units.is_empty() {
        start_units.extend(start_lines(&air_units));
    }
    start_units.extend(start_lines(&mint_units));

    // The logs job: the logs of the running services are kept under the cap.
    let mut logs: Vec<String> = Vec::new();
    let log = |name: &str| format!("{}/{name}.log", svc.log_dir);
    logs.push(log("psp-mint"));
    logs.push(log("esp-mint"));
    if spi.air.manage {
        logs.push(log("air_server"));
    }
    if manage {
        logs.push(log("bitcoind"));
    }
    if svc.health {
        logs.push(log("psp-mint-health"));
        logs.push(log("esp-mint-health"));
        if spi.air.manage {
            logs.push(log(&format!("{air_base}-health")));
        }
    }
    logs.push(schedule::log_file(spi).display().to_string());
    if !mac {
        logs_entry(spi, ops, &logs, &mut files, &mut installs)?;
        start_units.push(format!("sudo systemctl enable --now {LOGS_UNIT}.timer"));
    }

    // The script.
    let mut settings = String::new();
    for (key, value) in [
        ("user", svc.user.as_str()),
        ("group", svc.group.as_str()),
        ("bin_dir", svc.bin_dir.as_str()),
        ("etc_dir", svc.etc_dir.as_str()),
        ("data_dir", svc.data_dir.as_str()),
        ("log_dir", svc.log_dir.as_str()),
        ("unit_dir", unit_dir),
    ] {
        settings.push_str(&format!("{key}={}\n", sh_word(value)));
    }
    let script_vars = vec![
        ("settings", settings.trim_end().to_string()),
        (
            "folders",
            format!(
                "install -d -m 0750 -o \"$user\" -g \"$group\" {}",
                folders.join(" ")
            ),
        ),
        ("installs", installs.join("\n")),
    ];
    let script = expand(
        SCRIPT,
        if mac { SCRIPT_MAC } else { SCRIPT_LINUX },
        &script_vars,
        false,
    )?;
    files.push((SCRIPT.into(), script));

    Ok(Plan {
        system: if mac {
            "macOS (launchd)"
        } else {
            "Linux (systemd)"
        },
        files,
        warnings,
        notes,
        start: start_units,
        unit_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Network;

    #[test]
    fn a_word_for_sh_is_quoted_only_when_it_has_to_be() {
        assert_eq!(sh_word("/usr/local/bin/x"), "/usr/local/bin/x");
        assert_eq!(sh_word("/a b"), "'/a b'");
        assert_eq!(sh_word("/a'b"), "'/a'\\''b'");
    }

    #[test]
    fn the_rpc_port_is_read_from_the_url() {
        assert_eq!(url_port("http://127.0.0.1:18443"), Some(18443));
        assert_eq!(url_port("https://node.example:8332/wallet/x"), Some(8332));
        assert_eq!(url_port("http://node.example"), None);
        assert_eq!(url_port("http://[::1]"), None);
    }

    #[test]
    fn every_network_has_a_cookie_folder_and_a_flag() {
        for n in [
            Network::Main,
            Network::Regtest,
            Network::Signet,
            Network::Testnet,
        ] {
            assert_eq!(n.subdir().is_empty(), run::chain_flag(n).is_none());
        }
    }
}
