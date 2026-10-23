//! `spi upgrade`: bring one or all of bitcoind, air_server, psp-mint and
//! esp-mint to their target version, one at a time.
//!
//! **Brief downtime.** Every upgrade is a restart of the program
//! on the service install: the old process stops, the new build starts. Nothing
//! is upgraded while it runs. `Program::method` says, for each program, what the
//! downtime is and why. (Upgrades with no downtime are not built; see ROADMAP
//! item 25.)
//!
//! **The target.** A program's `version` in spi.toml is an optional pin. With no
//! pin the target is the newest tagged release the release source knows
//! (`release::newest_of`); with a pin it is the pin, which may be older than the
//! newest, and a newer release is reported ("pinned at x (newest y)"), never
//! installed. air_server publishes no releases (`Program::publishes`), so its
//! `version` and `bin` are the operator's, as before.
//!
//! **The build.** When the release source has the target, spi downloads the
//! release binary, checks its SHA-256 against the one the release publishes, and
//! makes it executable; a download that fails the check is deleted and nothing
//! is changed. Otherwise the build is the one `bin` names in spi.toml. Either
//! way its `--version` must be the target before anything is changed. spi never
//! builds from source.
//!
//! It upgrades the **service install** (`spi install` and its script): the
//! binary in `[service] bin_dir`. For one program:
//!
//! 1. the new binary is obtained and checked (`--version`);
//! 2. it is copied beside the installed one and swapped in, the old one kept as
//!    `<name>.previous`;
//! 3. the service is restarted;
//! 4. spi waits for it to be healthy and, for a mint, for `/health` to report the
//!    target `version`. When it is not, the old binary is put back and started,
//!    and the upgrade stops there.
//!
//! The next program is not touched until the one before is healthy. A program
//! that is already at its target is left alone and nothing is stopped.
//!
//! `commands::upgrade` warns and asks before anything is changed (a dry run
//! asks nothing and changes nothing). It needs the rights to write `bin_dir` and
//! to run the service manager, so run it as root (`sudo spi upgrade`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::Spi;
use crate::exec::Exec;
use crate::release::{self, Source};
use crate::service::find_binary;
use crate::versions::{self, same_version};

/// What a mint's restart costs, and why, in these words. Used for psp-mint and
/// esp-mint by `spi upgrade` and `spi restart`, and quoted by `spi reload`.
pub const MINT_DOWNTIME: &str = "BRIEF DOWNTIME: restart. One process holds the mint's data (sled's exclusive lock), so the new build starts only after the old one has stopped; wallets' AIR requests wait in the mailbox slots and are answered when it is back.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Bitcoind,
    Air,
    Psp,
    Esp,
}

impl Program {
    /// The order a full upgrade goes in: the order the services start.
    pub const ALL: [Program; 4] = [Program::Bitcoind, Program::Air, Program::Psp, Program::Esp];

    pub fn name(self) -> &'static str {
        match self {
            Program::Bitcoind => "bitcoind",
            Program::Air => "air_server",
            Program::Psp => "psp-mint",
            Program::Esp => "esp-mint",
        }
    }

    pub fn parse(text: &str) -> Option<Program> {
        Program::ALL.into_iter().find(|p| p.name() == text)
    }

    /// The unit (Linux) or launchd label (macOS) name, as `service::plan`
    /// writes them.
    pub fn base(self, mac: bool) -> &'static str {
        match (self, mac) {
            (Program::Bitcoind, true) => "bitcoind",
            (Program::Bitcoind, false) => "sp-bitcoind",
            (Program::Air, true) => "air-server",
            (Program::Air, false) => "sp-air-server",
            (Program::Psp, _) => "psp-mint",
            (Program::Esp, _) => "esp-mint",
        }
    }

    /// The pin: the `version` spi.toml names for this program, if any.
    pub fn pin(self, spi: &Spi) -> Option<&str> {
        match self {
            Program::Bitcoind => spi.bitcoind_version.as_deref(),
            Program::Air => spi.air.version.as_deref(),
            Program::Psp => spi.psp.version.as_deref(),
            Program::Esp => spi.esp.version.as_deref(),
        }
    }

    pub fn bin(self, spi: &Spi) -> &str {
        match self {
            Program::Bitcoind => &spi.bitcoind_bin,
            Program::Air => &spi.air.bin,
            Program::Psp => &spi.psp.bin,
            Program::Esp => &spi.esp.bin,
        }
    }

    pub fn managed(self, spi: &Spi) -> bool {
        match self {
            Program::Bitcoind => spi.manage_bitcoind,
            Program::Air => spi.air.manage,
            Program::Psp | Program::Esp => true,
        }
    }

    pub fn is_mint(self) -> bool {
        matches!(self, Program::Psp | Program::Esp)
    }

    /// Whether upstream publishes release binaries for this program. air_server
    /// (ramp-stack) publishes none; when it does, this is the one place that
    /// changes.
    pub fn publishes(self) -> bool {
        !matches!(self, Program::Air)
    }

    /// Where the service install keeps this program's binary.
    pub fn installed_path(self, spi: &Spi) -> PathBuf {
        PathBuf::from(&spi.service.bin_dir).join(self.name())
    }

    /// What the upgrade of this program is, said plainly.
    pub fn method(self) -> String {
        match self {
            Program::Bitcoind => {
                "restart (the mints keep serving; psp-mint's scan waits for the node)".into()
            }
            Program::Air => concat!(
                "BRIEF DOWNTIME: restart. air_server 1.0.1 cannot hand over without an upstream change (file:line below are in its src, tag v1.0.1). ",
                "One process owns the SQLite store and both sockets (deploy/README.md: exactly one active SQLite writer), ",
                "and wake-ups only reach sessions in the same process (server.rs:390-395), so nginx cannot stand in for a handover. ",
                "The listener is a plain bind (websocket.rs:169). Upstream would need a drain mode (stop accepting, cancel sessions, close the SQLite connection) ",
                "and a listening socket handed to the new process (socket activation or an inherited fd), after which the new process opens STORAGE.db (server.rs:419-463). ",
                "Nothing is lost meanwhile: a Create is stored with no expiry (server.rs:872-888) and a Read of a missing key is held per session (server.rs:588-610), ",
                "but a client process restart makes a new session (deploy/README.md:81-83), so the mints' AIR clients reconnect"
            )
            .into(),
            Program::Psp | Program::Esp => MINT_DOWNTIME.into(),
        }
    }
}

/// What a program's `/health` answered: whether it is up, and for a mint the
/// `version` it reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub up: bool,
    pub version: Option<String>,
}

impl Probe {
    /// Up, with no version (bitcoind, air_server).
    pub fn up() -> Probe {
        Probe {
            up: true,
            version: None,
        }
    }

    /// A mint that is up and reports `version`.
    pub fn mint(version: &str) -> Probe {
        Probe {
            up: true,
            version: Some(version.to_string()),
        }
    }
}

/// How an upgrade is run.
pub struct Options<'a> {
    pub dry_run: bool,
    /// How long each program gets to be healthy after its restart.
    pub wait_secs: u64,
    /// Where releases come from.
    pub source: &'a dyn Source,
}

/// What `upgrade` did.
#[derive(Debug, Default)]
pub struct Upgraded {
    pub lines: Vec<String>,
    pub ok: bool,
    /// The programs that were changed, or, in a dry run, would be.
    pub changes: Vec<Program>,
}

/// The command that restarts a service.
pub fn restart_command(program: Program, mac: bool) -> (String, Vec<String>) {
    let base = program.base(mac);
    if mac {
        (
            "launchctl".into(),
            vec![
                "kickstart".into(),
                "-k".into(),
                format!("system/org.servicepoints.{base}"),
            ],
        )
    } else {
        (
            "systemctl".into(),
            vec!["restart".into(), format!("{base}.service")],
        )
    }
}

/// The version a program reports for the binary at `path`, or why it cannot.
pub fn reported(
    spi: &Spi,
    exec: &dyn Exec,
    program: Program,
    path: &Path,
) -> Result<String, String> {
    if program == Program::Air {
        // air_server prints no version: only spi's record of this exact file
        // says what it is. (`[air] version` is what it should become.)
        return versions::air_recorded(spi, path)
            .ok_or_else(|| format!("spi has no record of the version of {}", path.display()));
    }
    let ran = exec.run(&path.display().to_string(), &["--version".to_string()], &[])?;
    if !ran.ok() {
        return Err(format!(
            "`{} --version` failed: {}",
            path.display(),
            ran.last_line()
        ));
    }
    let first = ran.stdout.lines().next().unwrap_or("");
    let found = match program {
        Program::Bitcoind => versions::bitcoind_version(first),
        _ => versions::program_version(program.name(), first),
    };
    found.ok_or_else(|| {
        format!(
            "`{} --version` printed `{first}`, which has no version",
            path.display()
        )
    })
}

/// Poll `probe` until `done` accepts what it answers, or `wait_secs` run out.
/// Returns the last answer and whether it was accepted.
fn wait_for(
    probe: &dyn Fn(Program) -> Probe,
    program: Program,
    wait_secs: u64,
    done: &dyn Fn(&Probe) -> bool,
) -> (Probe, bool) {
    let end = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        let answer = probe(program);
        if done(&answer) {
            return (answer, true);
        }
        if Instant::now() >= end {
            return (answer, false);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Whether a restarted program is serving as it should: up and, for a mint, on
/// `/health` reporting the version it was restarted into.
fn serving(program: Program, answer: &Probe, target: &str) -> bool {
    answer.up
        && (!program.is_mint()
            || answer
                .version
                .as_deref()
                .is_some_and(|v| same_version(v, target)))
}

/// Why a restarted program is not serving yet, for the line that says it was
/// rolled back.
fn not_serving(program: Program, answer: &Probe, target: &str, wait_secs: u64) -> String {
    match (&answer.version, answer.up && program.is_mint()) {
        (Some(v), true) => format!(
            "after {wait_secs} s /health reported version {v}, not {target}, so the new build is not the one serving"
        ),
        (None, true) => format!(
            "after {wait_secs} s /health reported no version, so it cannot be told to be {target}"
        ),
        _ => format!("the new build did not become healthy within {wait_secs} s"),
    }
}

/// Restart one program through the service manager.
pub fn restart(spi: &Spi, exec: &dyn Exec, program: Program) -> Result<(), String> {
    let (bin, args) = restart_command(program, spi.mac);
    let ran = exec.run(&bin, &args, &[])?;
    if ran.ok() {
        Ok(())
    } else {
        Err(format!(
            "`{}` failed: {}",
            crate::exec::command_line(&bin, &args),
            ran.last_line()
        ))
    }
}

/// Restart one program and wait until it is serving again (a mint: and
/// reporting some version). Used by `spi restart`.
pub fn restart_and_wait(
    spi: &Spi,
    exec: &dyn Exec,
    program: Program,
    wait_secs: u64,
    probe: &dyn Fn(Program) -> Probe,
) -> Result<(), String> {
    restart(spi, exec, program)?;
    let (_, up) = wait_for(probe, program, wait_secs, &|p| p.up);
    if up {
        Ok(())
    } else {
        Err(format!(
            "{} did not answer /health within {wait_secs} s of its restart",
            program.name()
        ))
    }
}

/// What spi will install for a program, and why.
struct Decision {
    target: String,
    /// "pinned at x (newest y)", when a newer release exists than the pin.
    pin_note: Option<String>,
    /// Whether the build is downloaded from the release source (else `bin`).
    from_release: bool,
}

/// The target of one program, or why there is none.
fn decide(spi: &Spi, program: Program, source: &dyn Source) -> Result<Decision, String> {
    let name = program.name();
    let pin = program.pin(spi);
    // A bitcoind the operator supplied (`bin`) is an override: no download.
    let own = program == Program::Bitcoind && spi.bitcoind_bin_set;
    let newest = if program.publishes() && !own {
        release::newest_of(source, program)
    } else {
        Ok(None)
    };
    match (pin, newest) {
        (Some(pin), Ok(Some(newest))) => Ok(Decision {
            target: pin.to_string(),
            pin_note: release::is_newer(&newest, pin)
                .then(|| format!("{name}: pinned at {pin} (newest {newest})")),
            from_release: true,
        }),
        (Some(pin), _) => Ok(Decision {
            target: pin.to_string(),
            pin_note: None,
            from_release: false,
        }),
        (None, Ok(Some(newest))) => Ok(Decision {
            target: newest,
            pin_note: None,
            from_release: true,
        }),
        (None, Ok(None)) if !program.publishes() || own => {
            Err("spi.toml names no version for it (set its `version`)".into())
        }
        (None, Ok(None)) => Err(
            "spi.toml names no version, and the release source lists no release for it (set its `version`)"
                .into(),
        ),
        (None, Err(e)) => Err(format!(
            "spi.toml names no version, and the newest release could not be found: {e}"
        )),
    }
}

/// Upgrade `programs`, in the order given, one at a time. With `dry_run`
/// nothing is changed and each program's plan is said. The first failure stops
/// the run.
pub fn upgrade(
    spi: &Spi,
    programs: &[Program],
    opts: &Options,
    exec: &dyn Exec,
    probe: &dyn Fn(Program) -> Probe,
) -> Upgraded {
    let mut out = Upgraded {
        lines: Vec::new(),
        ok: true,
        changes: Vec::new(),
    };
    let mut already = 0usize;
    for &program in programs {
        let name = program.name();
        if !program.managed(spi) {
            out.lines
                .push(format!("{name}: skipped: spi does not manage it"));
            continue;
        }
        let decision = match decide(spi, program, opts.source) {
            Ok(d) => d,
            Err(why) => {
                out.lines.push(format!("{name}: skipped: {why}"));
                continue;
            }
        };
        match upgrade_one(spi, program, &decision, opts, exec, probe) {
            Ok(done) => {
                out.lines.extend(done.lines);
                if done.changed {
                    out.changes.push(program);
                } else {
                    already += 1;
                }
            }
            Err(lines) => {
                out.lines.extend(lines);
                out.ok = false;
                out.lines
                    .push("stopped: the programs after this one were not touched".into());
                return out;
            }
        }
    }
    if out.ok && out.changes.is_empty() && already == programs.len() {
        out.lines
            .push("everything up to date: nothing was stopped".into());
    }
    out
}

struct One {
    lines: Vec<String>,
    /// False when the program was already at its target.
    changed: bool,
}

/// The build to install: downloaded and verified, or the one `bin` names.
fn obtain(
    spi: &Spi,
    program: Program,
    d: &Decision,
    installed: &Path,
    opts: &Options,
    exec: &dyn Exec,
) -> Result<PathBuf, String> {
    let name = program.name();
    let target = &d.target;
    let new = if d.from_release {
        let dir = release::releases_dir(spi, program, target);
        let got = opts
            .source
            .fetch(program, target, &dir)
            .map_err(|e| format!("cannot get {name} {target} from the release source: {e}"))?;
        let have = versions::sha256_file(&got.file)?;
        if !have.eq_ignore_ascii_case(&got.sha256) {
            let _ = fs::remove_file(&got.file);
            return Err(format!(
                "the download of {name} {target} failed its checksum (the release says {}, the file is {have}): it was deleted and nothing was changed",
                got.sha256
            ));
        }
        release::make_executable(&got.file)?;
        got.file
    } else {
        let Some(new) = find_binary(program.bin(spi)) else {
            return Err(format!(
                "cannot find the new build `{}` (set its `bin` in spi.toml to the new file)",
                program.bin(spi)
            ));
        };
        if fs::canonicalize(installed).ok().as_deref() == Some(new.as_path()) {
            return Err(format!(
                "`bin` names the installed file {} itself: point it at the new build",
                installed.display()
            ));
        }
        new
    };
    // The new build must be what is wanted. air_server cannot say, so it is
    // taken as named, unless it is the very file that is installed.
    if program == Program::Air {
        if versions::sha256_file(&new).ok() == versions::sha256_file(installed).ok() {
            return Err(format!(
                "the new build {} is the same file as the installed one, not {target}",
                new.display()
            ));
        }
    } else {
        match reported(spi, exec, program, &new) {
            Ok(found) if same_version(&found, target) => {}
            Ok(found) => {
                return Err(format!(
                    "{} is {found}, but {} names {target}: nothing was changed",
                    new.display(),
                    if d.from_release {
                        "the release"
                    } else {
                        "spi.toml"
                    }
                ));
            }
            Err(e) => return Err(format!("nothing was changed: {e}")),
        }
    }
    Ok(new)
}

fn upgrade_one(
    spi: &Spi,
    program: Program,
    d: &Decision,
    opts: &Options,
    exec: &dyn Exec,
    probe: &dyn Fn(Program) -> Probe,
) -> Result<One, Vec<String>> {
    let name = program.name();
    let target = d.target.as_str();
    let wait_secs = opts.wait_secs;
    let fail = |why: String| vec![format!("FAIL: {name}: {why}")];
    let mut lines = Vec::new();
    if let Some(note) = &d.pin_note {
        lines.push(note.clone());
    }
    let installed = program.installed_path(spi);
    if !installed.is_file() {
        return Err(fail(format!(
            "{} does not exist: `spi upgrade` upgrades the service install, so run `spi install` and its script first",
            installed.display()
        )));
    }
    let current = reported(spi, exec, program, &installed).ok();
    if current.as_deref().is_some_and(|c| same_version(c, target)) {
        lines.push(format!("{name}: already {target}: nothing to do"));
        return Ok(One {
            lines,
            changed: false,
        });
    }
    let from = current.unwrap_or_else(|| "an unknown version".into());
    // A dry run downloads nothing: it says where the build would come from.
    let new = if opts.dry_run && d.from_release {
        None
    } else {
        match obtain(spi, program, d, &installed, opts, exec) {
            Ok(n) => Some(n),
            Err(why) => {
                lines.push(format!("FAIL: {name}: {why}"));
                return Err(lines);
            }
        }
    };
    lines.push(format!(
        "{name}: {from} -> {target} by {}",
        program.method()
    ));
    if opts.dry_run {
        if new.is_none() {
            lines.push(format!(
                "{name}: the build would be downloaded from the release source and checked against its SHA-256"
            ));
        }
        lines.push(format!("{name}: dry run: nothing was changed"));
        return Ok(One {
            lines,
            changed: true,
        });
    }
    let Some(new) = new else {
        return Err(fail("no build to install".into()));
    };

    let staged = installed.with_file_name(format!("{name}.new"));
    let previous = installed.with_file_name(format!("{name}.previous"));
    let swap = || -> Result<(), String> {
        fs::copy(&new, &staged).map_err(|e| {
            format!(
                "cannot write {}: {e} (run spi upgrade as root)",
                staged.display()
            )
        })?;
        fs::rename(&installed, &previous).map_err(|e| {
            format!(
                "cannot keep {} as {}: {e}",
                installed.display(),
                previous.display()
            )
        })?;
        if let Err(e) = fs::rename(&staged, &installed) {
            let _ = fs::rename(&previous, &installed);
            return Err(format!("cannot put {} in place: {e}", installed.display()));
        }
        Ok(())
    };
    if let Err(e) = swap() {
        let _ = fs::remove_file(&staged);
        lines.push(format!("FAIL: {name}: {e}"));
        return Err(lines);
    }
    lines.push(format!(
        "{name}: installed {}, the old one is {}",
        installed.display(),
        previous.display()
    ));

    let restarted = restart(spi, exec, program);
    let (last, went) = if restarted.is_ok() {
        wait_for(probe, program, wait_secs, &|p| serving(program, p, target))
    } else {
        (Probe::default(), false)
    };
    if !went {
        // Put the old binary back and start it.
        let why = match &restarted {
            Err(e) => e.clone(),
            Ok(()) => not_serving(program, &last, target, wait_secs),
        };
        let back = fs::rename(&previous, &installed);
        let again = back.is_ok() && restart(spi, exec, program).is_ok();
        lines.push(match (back.is_ok(), again) {
            (true, true) => format!("FAIL: {name}: {why}; the old one is back and restarted"),
            (true, false) => format!(
                "FAIL: {name}: {why}, and the old one is back in place but could not be restarted: restart {} yourself",
                program.base(spi.mac)
            ),
            (false, _) => format!(
                "FAIL: {name}: {why}, and the old one could not be put back: it is {}",
                previous.display()
            ),
        });
        return Err(lines);
    }
    if program == Program::Air
        && let Err(e) = versions::record_air(spi, &installed, target)
    {
        lines.push(format!("WARNING: {name}: {e}"));
    }
    if d.from_release
        && let Err(e) = release::record_installed(spi, program, target, &installed)
    {
        lines.push(format!("WARNING: {name}: {e}"));
    }
    lines.push(format!("{name}: now {target}, healthy"));
    if d.from_release {
        match release::prune(spi, program, &[target, &from]) {
            Ok(gone) if !gone.is_empty() => lines.push(format!(
                "{name}: removed old downloads: {}",
                gone.join(", ")
            )),
            Ok(_) => {}
            Err(e) => lines.push(format!("WARNING: {name}: {e}")),
        }
    }
    Ok(One {
        lines,
        changed: true,
    })
}
