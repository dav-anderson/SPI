//! The files that run psp-mint, esp-mint and bitcoind as services on a machine
//! that boots, generated from `spi.toml` and nothing else.
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
//! in `_xml` are inserted as they are.
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

use crate::config::{Spi, Which};
use crate::{generate, run};

const MINT_SERVICE: &str = include_str!("../ops/systemd/mint.service");
const HEALTH_SERVICE: &str = include_str!("../ops/systemd/health.service");
const BITCOIND_SERVICE: &str = include_str!("../ops/systemd/bitcoind.service");
const MINT_PLIST: &str = include_str!("../ops/launchd/mint.plist");
const HEALTH_PLIST: &str = include_str!("../ops/launchd/health.plist");
const BITCOIND_PLIST: &str = include_str!("../ops/launchd/bitcoind.plist");
const HEALTH_CONF: &str = include_str!("../ops/health.conf");
const SCRIPT_LINUX: &str = include_str!("../ops/install-system.linux.sh");
const SCRIPT_MAC: &str = include_str!("../ops/install-system.mac.sh");

/// The health monitor, as installed.
pub const HEALTH_MONITOR: &str = include_str!("../ops/sp-health-monitor.sh");

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

/// Whether a path is in a home folder, where a service can not go unless
/// `ProtectHome` is relaxed.
pub fn under_home(path: &str) -> bool {
    path == "/root"
        || ["/home/", "/root/", "/Users/"]
            .iter()
            .any(|p| path.starts_with(p))
}

fn protect_home(paths: &[&str]) -> &'static str {
    if paths.iter().any(|p| under_home(p)) {
        "read-only"
    } else {
        "yes"
    }
}

/// A value that may go into a unit, a property list and the script as it is.
fn plain(what: &str, value: &str) -> Result<(), String> {
    let ok = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._+@:=,-".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{what} is `{value}`: in a service file it may hold only letters, digits and / . _ + @ : = , - (no spaces, quotes, % or $). Use a path without them"
        ))
    }
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

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// `template` with each `{{key}}` replaced by its value in `vars`. With `xml`
/// the values are escaped, except those of keys that end in `_xml`.
pub fn expand(
    what: &str,
    template: &str,
    vars: &[(&str, String)],
    xml: bool,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(format!("{what}: a `{{{{` is never closed with `}}}}`"));
        };
        let key = after[..end].trim();
        let Some((_, value)) = vars.iter().find(|(k, _)| *k == key) else {
            let known: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
            return Err(format!(
                "{what}: unknown placeholder {{{{{key}}}}} (known here: {})",
                known.join(", ")
            ));
        };
        if xml && !key.ends_with("_xml") {
            out.push_str(&xml_escape(value));
        } else {
            out.push_str(value);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The binary `spec` names (a path, or a name on PATH), as an absolute path.
pub fn find_binary(spec: &str) -> Option<PathBuf> {
    let found = if spec.contains('/') {
        let p = PathBuf::from(spec);
        p.is_file().then_some(p)
    } else {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(spec))
            .find(|p| p.is_file())
    };
    found.and_then(|p| fs::canonicalize(p).ok())
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
    if let Some(port) = url_port(&spi.rpc_url) {
        if port != spi.network.rpc_port() {
            args.push(format!("-rpcport={port}"));
        }
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

fn logrotate(spi: &Spi) -> String {
    format!(
        "# Written by `spi install`. copytruncate is needed because the mints keep the file open.\n\
         {}/*.log {{\n    weekly\n    rotate 8\n    compress\n    missingok\n    copytruncate\n}}\n",
        spi.service.log_dir
    )
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

/// Every file for the service: see the module documentation.
pub fn plan(spi: &Spi, ops: Option<&Path>) -> Result<Plan, String> {
    if let Some(dir) = ops {
        if !dir.is_dir() {
            return Err(format!("--ops {} is not a folder", dir.display()));
        }
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
    let mut binaries = vec![
        ("psp-mint", spi.psp.bin.as_str()),
        ("esp-mint", spi.esp.bin.as_str()),
    ];
    if manage {
        binaries.push(("bitcoind", spi.bitcoind_bin.as_str()));
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
                if manage && !mac {
                    "network-online.target sp-bitcoind.service"
                } else {
                    "network-online.target"
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
            let health_paths = [
                svc.bin_dir.as_str(),
                svc.etc_dir.as_str(),
                svc.log_dir.as_str(),
            ];
            let mut vars = common_vars(spi, protect_home(&health_paths));
            vars.push(("name", name.to_string()));
            vars.push(("health_url", format!("{}/health", spi.url(which))));
            vars.push((
                "restart_cmd",
                if mac {
                    format!("launchctl kickstart -k system/org.servicepoints.{name}")
                } else {
                    format!("systemctl restart {name}.service")
                },
            ));
            let conf = format!("{name}-health.conf");
            files.push((conf.clone(), expand(&conf, HEALTH_CONF, &vars, false)?));
            installs.push(install_line(
                "keep",
                "0644",
                "root",
                root_group,
                &format!("\"$here/{conf}\""),
                &format!("\"$etc_dir/{conf}\""),
            ));
            let file = unit_name(&format!("{name}-health"));
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

    // How the services are started, in order. bitcoind comes first and alone,
    // and the wallet is set up once it answers, so psp-mint finds it.
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
    start_units.extend(start_lines(&mint_units));

    if !mac {
        files.push(("logrotate-sp-mint".into(), logrotate(spi)));
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

    fn vars() -> Vec<(&'static str, String)> {
        vec![("a", "1 & 2".into()), ("b_xml", "<x/>".into())]
    }

    #[test]
    fn placeholders_are_replaced_and_text_around_them_is_left_alone() {
        let out = expand("t", "x {{a}} y {{ a }} ${z} {not}", &vars(), false).unwrap();
        assert_eq!(out, "x 1 & 2 y 1 & 2 ${z} {not}");
    }

    #[test]
    fn xml_values_are_escaped_except_the_ones_that_say_they_are_xml() {
        let out = expand("t", "<s>{{a}}</s>{{b_xml}}", &vars(), true).unwrap();
        assert_eq!(out, "<s>1 &amp; 2</s><x/>");
    }

    #[test]
    fn an_unknown_or_unclosed_placeholder_is_an_error_that_names_it() {
        let e = expand("the unit", "{{nope}}", &vars(), false).unwrap_err();
        assert!(e.contains("the unit") && e.contains("{{nope}}") && e.contains("a, b_xml"));
        let e = expand("the unit", "x {{a", &vars(), false).unwrap_err();
        assert!(e.contains("never closed"));
    }

    #[test]
    fn only_plain_values_go_into_a_unit() {
        assert!(plain("p", "/usr/local/bin").is_ok());
        assert!(plain("p", "-datadir=/var/lib/x_y-1.2").is_ok());
        for bad in [
            "", "/a b", "/a'b", "/a\"b", "/a%b", "/a$b", "/a\\b", "/a\nb", "/a;b",
        ] {
            assert!(plain("p", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_word_for_sh_is_quoted_only_when_it_has_to_be() {
        assert_eq!(sh_word("/usr/local/bin/x"), "/usr/local/bin/x");
        assert_eq!(sh_word("/a b"), "'/a b'");
        assert_eq!(sh_word("/a'b"), "'/a'\\''b'");
    }

    #[test]
    fn home_folders_are_found() {
        for p in [
            "/home/x",
            "/home/x/y",
            "/root",
            "/root/x",
            "/Users/x/Library",
        ] {
            assert!(under_home(p), "{p}");
        }
        for p in ["/var/lib/sp-mint", "/homework", "/usr/local/bin", "/Users"] {
            assert!(!under_home(p), "{p}");
        }
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
