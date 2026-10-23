//! Releases: which versions of bitcoind, air_server, psp-mint and esp-mint
//! exist upstream, which is the newest, and how a build is obtained and
//! checked. `spi upgrade check` reads it; `spi upgrade` and `spi install` use it
//! to pick the target and to get the build.
//!
//! **The source is a trait.** [`Source`] answers two questions: which version
//! tags a program has published, and, for one version, where its release binary
//! is (downloaded into a folder spi gives it) and what SHA-256 the release
//! publishes for it. spi does the rest itself: it ignores pre-release and
//! non-version tags, picks the newest, compares the downloaded file's SHA-256
//! with the published one and refuses a build that differs. The real source is
//! [`crate::fetch::CurlSource`] (GitHub releases and bitcoincore.org, through
//! `curl`). [`NoSource`] knows nothing and is only for tests that need no
//! upstream; the other tests use a source of their own that serves files from
//! a folder.
//!
//! **Versions.** A tag is `x.y.z` or `x.y` (Bitcoin Core), with or without a
//! leading `v`. A tag with anything else in it (`0.7.0-rc1`, `29.0rc1`) is a
//! pre-release or not a version, and is ignored. `x.y` and `x.y.0` are the same
//! number.
//!
//! **What spi records.** After a build from a release is installed and healthy,
//! spi writes `<spi folder>/installed/<program>.json` with the version and the
//! SHA-256 of the installed file, as `air/installed.json` does for air_server.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Spi;
use crate::exec::Exec;
use crate::service::find_binary;
use crate::upgrade::{self, Program};
use crate::versions::{self, Installed};

/// A release binary that was downloaded, and the SHA-256 its release publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    pub file: PathBuf,
    pub sha256: String,
}

/// Where releases come from.
pub trait Source {
    /// The version tags `program` has published, as they are tagged (`v0.7.0`).
    /// `Err` with the reason when they cannot be listed.
    fn tags(&self, program: Program) -> Result<Vec<String>, String>;

    /// Download the release binary of `program` at `version` for this machine
    /// into the folder `dir` (made if need be). `Err` with the reason when there
    /// is none or it cannot be downloaded.
    fn fetch(&self, program: Program, version: &str, dir: &Path) -> Result<Download, String>;
}

/// No release source: nothing is known upstream.
pub struct NoSource;

impl Source for NoSource {
    fn tags(&self, _program: Program) -> Result<Vec<String>, String> {
        Err("no release source is configured in this spi".into())
    }

    fn fetch(&self, _program: Program, _version: &str, _dir: &Path) -> Result<Download, String> {
        Err("no release source is configured in this spi".into())
    }
}

/// The numbers of a release tag: `x.y` or `x.y.z`, digits only, with an
/// optional leading `v`. `None` for anything else.
fn numbers(tag: &str) -> Option<[u64; 3]> {
    let text = tag.strip_prefix('v').unwrap_or(tag);
    let mut out = Vec::new();
    for part in text.split('.') {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        out.push(part.parse::<u64>().ok()?);
    }
    if !(2..=3).contains(&out.len()) {
        return None;
    }
    out.resize(3, 0);
    Some([out[0], out[1], out[2]])
}

/// Whether `a` is a higher version than `b`. A tag that is not a version is
/// never newer than anything.
pub fn is_newer(a: &str, b: &str) -> bool {
    match (numbers(a), numbers(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// The newest release among `tags`, without its `v`. Pre-release and
/// non-version tags are ignored.
pub fn newest(tags: &[String]) -> Option<String> {
    tags.iter()
        .filter_map(|t| numbers(t).map(|n| (n, t.strip_prefix('v').unwrap_or(t).to_string())))
        .max_by_key(|(n, _)| *n)
        .map(|(_, t)| t)
}

/// The newest release `source` knows for `program`. `Ok(None)` when it lists
/// none that is a release; `Err` when it cannot be asked.
pub fn newest_of(source: &dyn Source, program: Program) -> Result<Option<String>, String> {
    Ok(newest(&source.tags(program)?))
}

/// Where a downloaded release is kept.
pub fn releases_dir(spi: &Spi, program: Program, version: &str) -> PathBuf {
    spi.dir.join("releases").join(program.name()).join(version)
}

/// Remove old downloads of `program` from `releases/`: the `release.keep` newest
/// versions stay, and so do the versions in `protect` (the installed one and the
/// one it replaced). A folder whose name is not a version is left alone. Returns
/// the versions removed.
pub fn prune(spi: &Spi, program: Program, protect: &[&str]) -> Result<Vec<String>, String> {
    let root = spi.dir.join("releases").join(program.name());
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut found: Vec<([u64; 3], String)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            numbers(&name).map(|n| (n, name))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut gone = Vec::new();
    for (i, (_, name)) in found.iter().enumerate() {
        if i < spi.release.keep as usize || protect.iter().any(|p| versions::same_version(p, name))
        {
            continue;
        }
        let path = root.join(name);
        fs::remove_dir_all(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
        gone.push(name.clone());
    }
    Ok(gone)
}

/// The record of what spi installed from a release.
pub fn record_path(spi: &Spi, program: Program) -> PathBuf {
    spi.dir
        .join("installed")
        .join(format!("{}.json", program.name()))
}

/// Write the record: `binary` is the file now installed and `version` what it is.
pub fn record_installed(
    spi: &Spi,
    program: Program,
    version: &str,
    binary: &Path,
) -> Result<(), String> {
    let record = Installed {
        version: version.to_string(),
        sha256: versions::sha256_file(binary)?,
    };
    let path = record_path(spi, program);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Mark a downloaded file executable (mode 0755).
pub fn make_executable(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot make {} executable: {e}", path.display()))
}

/// One line of `spi upgrade check`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckLine {
    pub name: String,
    /// The version of the installed program, or `unknown`.
    pub installed: String,
    /// The `version` in spi.toml, or `none`.
    pub spi_toml: String,
    /// The newest tagged release, `unknown`, or `none` when upstream publishes
    /// no releases.
    pub newest: String,
    /// `up_to_date`, `upgrade_available`, `pinned`, `no_releases`, `unmanaged` or
    /// `unknown`.
    pub status: String,
    /// The same in words, with the reason when it is unknown.
    pub detail: String,
}

/// The installed program: the service install's copy, else the build `bin` names.
fn installed_version(spi: &Spi, exec: &dyn Exec, program: Program) -> Result<String, String> {
    let service = program.installed_path(spi);
    if service.is_file() {
        return upgrade::reported(spi, exec, program, &service);
    }
    match find_binary(program.bin(spi)) {
        Some(path) => upgrade::reported(spi, exec, program, &path),
        None => Err(format!(
            "neither {} nor `{}` exists",
            service.display(),
            program.bin(spi)
        )),
    }
}

/// `spi upgrade check`: one line per program. Reads and runs `--version` only.
pub fn check(spi: &Spi, exec: &dyn Exec, source: &dyn Source) -> Vec<CheckLine> {
    Program::ALL
        .into_iter()
        .map(|program| check_one(spi, exec, source, program))
        .collect()
}

fn check_one(spi: &Spi, exec: &dyn Exec, source: &dyn Source, program: Program) -> CheckLine {
    let installed = installed_version(spi, exec, program);
    let pin = program.pin(spi);
    let line = |newest: &str, status: &str, detail: String| CheckLine {
        name: program.name().to_string(),
        installed: installed.clone().unwrap_or_else(|_| "unknown".into()),
        spi_toml: pin.unwrap_or("none").to_string(),
        newest: newest.to_string(),
        status: status.to_string(),
        detail,
    };
    if !program.publishes() {
        return line(
            "none",
            "no_releases",
            "no releases published upstream".into(),
        );
    }
    if !program.managed(spi) {
        return line(
            "none",
            "unmanaged",
            "not managed by spi (manage = false)".into(),
        );
    }
    let newest = match newest_of(source, program) {
        Ok(n) => n,
        Err(why) => return line("unknown", "unknown", format!("unknown ({why})")),
    };
    let Some(newest) = newest else {
        return line(
            "unknown",
            "unknown",
            "unknown (the release source lists no release for it)".into(),
        );
    };
    let installed = match &installed {
        Ok(v) => v.clone(),
        Err(why) => {
            return line(
                &newest,
                "unknown",
                format!("unknown (the installed version cannot be read: {why})"),
            );
        }
    };
    let target = pin.unwrap_or(&newest);
    if versions::same_version(&installed, target) {
        match pin {
            Some(p) if is_newer(&newest, p) => line(
                &newest,
                "pinned",
                format!("pinned at {p} (newest {newest})"),
            ),
            _ => line(&newest, "up_to_date", "up to date".into()),
        }
    } else {
        line(
            &newest,
            "upgrade_available",
            format!("upgrade available ({target})"),
        )
    }
}

/// The last line of `spi upgrade check`.
pub fn summary(lines: &[CheckLine]) -> String {
    let count = |status: &str| lines.iter().filter(|l| l.status == status).count();
    format!(
        "summary: {} up to date, {} upgrade available, {} pinned, {} unknown, {} without upstream releases",
        count("up_to_date"),
        count("upgrade_available"),
        count("pinned"),
        count("unknown"),
        count("no_releases")
    )
}

/// Text for a person, one line per program, then the summary.
pub fn render(lines: &[CheckLine]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(&format!(
            "{:<10} installed {:<9} spi.toml {:<9} newest {:<9} {}\n",
            l.name, l.installed, l.spi_toml, l.newest, l.detail
        ));
    }
    out.push_str(&summary(lines));
    out.push('\n');
    out
}

/// A program pinned to a version that is not known to be the newest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    pub program: Program,
    pub pin: String,
    /// The newest release; `None` when the source could not say.
    pub newest: Option<String>,
}

/// The pinned programs among `programs` whose pin is older than the newest
/// release, or whose newest release cannot be found. A program upstream
/// publishes nothing for (air_server) has no pin in this sense: its `version` is
/// the operator's word for the binary.
pub fn pinned(spi: &Spi, source: &dyn Source, programs: &[Program]) -> Vec<Pinned> {
    programs
        .iter()
        .filter(|p| p.publishes())
        .filter_map(|&program| {
            let pin = program.pin(spi)?.to_string();
            let newest = newest_of(source, program).ok().flatten();
            let held = newest.as_deref().is_none_or(|n| is_newer(n, &pin));
            held.then_some(Pinned {
                program,
                pin,
                newest,
            })
        })
        .collect()
}

/// The warning that ends `spi upgrade check` and `spi upgrade status` and opens
/// `spi upgrade`: every pinned program, its pin and the newest release. Empty
/// when nothing is pinned behind the newest.
pub fn pin_warning(held: &[Pinned]) -> Vec<String> {
    if held.is_empty() {
        return Vec::new();
    }
    let mut lines =
        vec!["WARNING: these programs are pinned and are not at the newest release:".to_string()];
    for h in held {
        lines.push(format!(
            "  {}: pinned at {} (newest {})",
            h.program.name(),
            h.pin,
            h.newest.as_deref().unwrap_or("unknown")
        ));
    }
    lines.push(
        "`spi upgrade` installs the pin unless it is reset. It asks about each pin (--keep-pins keeps them all, --reset-pins resets them all); a reset removes the `version` line from spi.toml (the file is backed up first).".to_string(),
    );
    lines
}

/// `text` (a spi.toml) without the `version` line of `[section]`, the rest as it
/// is, comments and all. `None` when that section has no such line.
pub fn without_pin_line(text: &str, section: &str) -> Option<String> {
    let mut current = String::new();
    let mut out = String::with_capacity(text.len());
    let mut removed = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with('[') && !t.starts_with("[[") {
            current = t
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
        } else if !removed
            && current == section
            && t.strip_prefix("version")
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        {
            removed = true;
            continue;
        }
        out.push_str(line);
    }
    removed.then_some(out)
}

/// The section of spi.toml that holds a program's `version`.
pub fn section_of(program: Program) -> &'static str {
    match program {
        Program::Bitcoind => "bitcoind",
        Program::Air => "air",
        Program::Psp => "psp",
        Program::Esp => "esp",
    }
}

/// A copy of `spi` with the pins of `programs` removed, so that the newest release is the target.
pub fn unpinned(spi: &Spi, programs: &[Program]) -> Spi {
    let mut out = spi.clone();
    for p in programs {
        match p {
            Program::Bitcoind => out.bitcoind_version = None,
            Program::Air => out.air.version = None,
            Program::Psp => out.psp.version = None,
            Program::Esp => out.esp.version = None,
        }
    }
    out
}

/// Keep the newest 5 `<name>.<epoch>.bak` files next to `config`.
fn prune_backups(config: &Path, name: &str) {
    let dir = config.parent().filter(|d| !d.as_os_str().is_empty());
    let mut found: Vec<(i64, PathBuf)> = fs::read_dir(dir.unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().to_string();
            let epoch = file
                .strip_prefix(name)?
                .strip_prefix('.')?
                .strip_suffix(".bak")?;
            Some((epoch.parse::<i64>().ok()?, e.path()))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, path) in found.into_iter().skip(5) {
        let _ = fs::remove_file(path);
    }
}

/// Remove the pins of `programs` from the settings file `config`, after copying
/// it to `<config>.<epoch>.bak`. Returns what was done, a line per program.
pub fn reset_pins(config: &Path, programs: &[Program], epoch: i64) -> Result<Vec<String>, String> {
    let text =
        fs::read_to_string(config).map_err(|e| format!("cannot read {}: {e}", config.display()))?;
    let mut edited = text.clone();
    let mut lines = Vec::new();
    let mut missing = Vec::new();
    for &program in programs {
        match without_pin_line(&edited, section_of(program)) {
            Some(next) => edited = next,
            None => missing.push(program.name()),
        }
    }
    if edited != text {
        let name = config
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "spi.toml".into());
        let backup = config.with_file_name(format!("{name}.{epoch}.bak"));
        fs::copy(config, &backup)
            .map_err(|e| format!("cannot back up {}: {e}", config.display()))?;
        fs::write(config, &edited)
            .map_err(|e| format!("cannot write {}: {e}", config.display()))?;
        prune_backups(config, &name);
        lines.push(format!(
            "reset: removed the `version` pin of {} from {} (the old file is {})",
            programs
                .iter()
                .map(|p| p.name())
                .filter(|n| !missing.contains(n))
                .collect::<Vec<_>>()
                .join(", "),
            config.display(),
            backup.display()
        ));
    }
    for m in missing {
        lines.push(format!(
            "WARNING: {m}: there is no `version` line under [{}] in {} to remove (a pin set in the environment is yours to unset); the newest release is still installed this time",
            section_of(Program::parse(m).unwrap_or(Program::Psp)),
            config.display()
        ));
    }
    Ok(lines)
}
