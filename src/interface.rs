//! spi's side of air-minterface: what the shared code needs to know about this
//! operator ([`Operator`], implemented on [`Spi`]) and about each program
//! ([`Managed`], implemented on [`Program`]). The shared code does the rest;
//! the sentences that are spi's own wording are said here, so the output is
//! what it has always been.

use std::path::{Path, PathBuf};

use air_minterface::exec::Exec;
use air_minterface::files::find_binary;
use air_minterface::record;
use air_minterface::schedule::Job;
use air_minterface::version::{Tags, program_version};
use air_minterface::{Managed, Operator, Probe, Say};

use crate::config::Spi;
use crate::upgrade::{self, Program};
use crate::versions;

impl Operator for Spi {
    const NAME: &'static str = "spi";
    const CONFIG_NAME: &'static str = "spi.toml";
    const UPGRADE_JOB: Job = Job {
        unit: "sp-upgrade",
        label: "org.servicepoints.upgrade",
        service: include_str!("../ops/systemd/upgrade.service"),
        timer: include_str!("../ops/systemd/upgrade.timer"),
        plist: include_str!("../ops/launchd/upgrade.plist"),
        all_programs: "bitcoind, air_server, psp-mint and esp-mint (all four)",
        log_name: "spi-upgrade.log",
    };

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn home(&self) -> &str {
        &self.home
    }

    fn mac(&self) -> bool {
        self.mac
    }

    fn log_dir(&self) -> PathBuf {
        PathBuf::from(&self.service.log_dir)
    }

    fn unit_dir(&self) -> PathBuf {
        PathBuf::from(if self.mac {
            "/Library/LaunchDaemons"
        } else {
            "/etc/systemd/system"
        })
    }

    fn release_url(&self) -> String {
        self.release.url.clone()
    }

    fn release_keep(&self) -> usize {
        self.release.keep as usize
    }
}

/// Where spi records what it installed of `program`: air_server's record is
/// `<air folder>/installed.json`, the others' are `<spi folder>/installed/<name>.json`.
pub fn record_path(spi: &Spi, program: Program) -> PathBuf {
    match program {
        Program::Air => versions::air_record_path(spi),
        _ => spi
            .dir
            .join("installed")
            .join(format!("{}.json", program.name())),
    }
}

impl Managed for Program {
    type Op = Spi;

    fn all() -> &'static [Program] {
        &Program::ALL
    }

    fn name(self) -> &'static str {
        match self {
            Program::Bitcoind => "bitcoind",
            Program::Air => "air_server",
            Program::Psp => "psp-mint",
            Program::Esp => "esp-mint",
        }
    }

    /// The `spi.toml` section that holds the program's `bin` and `version`.
    fn section(self) -> &'static str {
        match self {
            Program::Bitcoind => "bitcoind",
            Program::Air => "air",
            Program::Psp => "psp",
            Program::Esp => "esp",
        }
    }

    /// Whether upstream publishes release binaries for this program. air_server
    /// (ramp-stack) publishes none; when it does, this is the one place that
    /// changes.
    fn publishes(self) -> bool {
        !matches!(self, Program::Air)
    }

    /// Bitcoin Core tags `29.1` as well as `29.1.0`; the mints tag `x.y.z`.
    fn tags(self) -> Tags {
        Tags::Loose
    }

    /// Where the service install keeps this program's binary.
    fn installed_path(self, spi: &Spi) -> PathBuf {
        PathBuf::from(&spi.service.bin_dir).join(self.name())
    }

    fn bin(self, spi: &Spi) -> String {
        match self {
            Program::Bitcoind => spi.bitcoind_bin.clone(),
            Program::Air => spi.air.bin.clone(),
            Program::Psp => spi.psp.bin.clone(),
            Program::Esp => spi.esp.bin.clone(),
        }
    }

    /// The pin: the `version` spi.toml names for this program, if any.
    fn pin(self, spi: &Spi) -> Option<String> {
        match self {
            Program::Bitcoind => spi.bitcoind_version.clone(),
            Program::Air => spi.air.version.clone(),
            Program::Psp => spi.psp.version.clone(),
            Program::Esp => spi.esp.version.clone(),
        }
    }

    fn managed(self, spi: &Spi) -> bool {
        match self {
            Program::Bitcoind => spi.manage_bitcoind,
            Program::Air => spi.air.manage,
            Program::Psp | Program::Esp => true,
        }
    }

    /// A bitcoind the operator supplied (`bin`) is an override: no download.
    fn own_build(self, spi: &Spi) -> bool {
        self == Program::Bitcoind && spi.bitcoind_bin_set
    }

    /// air_server prints no version: only spi's record of the exact file says
    /// what it is.
    fn prints_version(self) -> bool {
        self != Program::Air
    }

    /// The installed program: the service install's copy, else the build `bin`
    /// names.
    fn installed_version(self, spi: &Spi, exec: &dyn Exec) -> Result<String, String> {
        let service = self.installed_path(spi);
        if service.is_file() {
            return self.reported(spi, exec, &service);
        }
        match find_binary(&self.bin(spi)) {
            Some(path) => self.reported(spi, exec, &path),
            None => Err(format!(
                "neither {} nor `{}` exists",
                service.display(),
                self.bin(spi)
            )),
        }
    }

    /// The version a program reports for the binary at `path`, or why it cannot.
    fn reported(self, spi: &Spi, exec: &dyn Exec, path: &Path) -> Result<String, String> {
        if self == Program::Air {
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
        let found = match self {
            Program::Bitcoind => versions::bitcoind_version(first),
            _ => program_version(self.name(), first),
        };
        found.ok_or_else(|| {
            format!(
                "`{} --version` printed `{first}`, which has no version",
                path.display()
            )
        })
    }

    /// A mint is serving the build it was restarted into when its `/health`
    /// reports that version; the others when they are up.
    fn serving(self, probe: &Probe, target: &str) -> bool {
        probe.up
            && (!self.is_mint()
                || probe
                    .version
                    .as_deref()
                    .is_some_and(|v| air_minterface::version::same_version(v, target)))
    }

    fn not_serving(self, probe: &Probe, target: &str, wait_secs: u64) -> String {
        match (&probe.version, probe.up && self.is_mint()) {
            (Some(v), true) => format!(
                "after {wait_secs} s /health reported version {v}, not {target}, so the new build is not the one serving"
            ),
            (None, true) => format!(
                "after {wait_secs} s /health reported no version, so it cannot be told to be {target}"
            ),
            _ => format!("the new build did not become healthy within {wait_secs} s"),
        }
    }

    fn restart(self, spi: &Spi, exec: &dyn Exec) -> Result<(), String> {
        upgrade::restart(spi, exec, self)
    }

    fn service_name(self, spi: &Spi) -> String {
        self.base(spi.mac).to_string()
    }

    /// air_server's record is kept whatever its source; the others' only for a
    /// build that came from a release.
    fn record(
        self,
        spi: &Spi,
        from_release: bool,
        version: &str,
        installed: &Path,
    ) -> Result<(), String> {
        if self == Program::Air || from_release {
            record::write(&record_path(spi, self), version, installed)
        } else {
            Ok(())
        }
    }

    fn say(self, _spi: &Spi, what: Say<'_>) -> String {
        match what {
            Say::NoReleases => "no releases published upstream".into(),
            Say::UnmanagedStatus => "not managed by spi (manage = false)".into(),
            Say::Unmanaged => "spi does not manage it".into(),
            Say::NoVersionUnpublished => {
                "spi.toml names no version for it (set its `version`)".into()
            }
            Say::NoVersionNoRelease => {
                "spi.toml names no version, and the release source lists no release for it (set its `version`)"
                    .into()
            }
            Say::NoVersionNotFound(why) => format!(
                "spi.toml names no version, and the newest release could not be found: {why}"
            ),
            Say::NotInstalled(path) => format!(
                "{} does not exist: `spi upgrade` upgrades the service install, so run `spi install` and its script first",
                path.display()
            ),
            Say::BinMissing(bin) => format!(
                "cannot find the new build `{bin}` (set its `bin` in spi.toml to the new file)"
            ),
            Say::BinIsInstalled(path) => format!(
                "`bin` names the installed file {} itself: point it at the new build",
                path.display()
            ),
            Say::Plan { from, target } => {
                format!("{}: {from} -> {target} by {}", self.name(), self.method())
            }
            Say::DryRunDownload => format!(
                "{}: the build would be downloaded from the release source and checked against its SHA-256",
                self.name()
            ),
            Say::Done { target } => format!("{}: now {target}, healthy", self.name()),
            Say::RunAs => "spi upgrade".into(),
        }
    }
}

/// A copy of `spi` with the pins of `programs` removed, so that the newest
/// release is the target.
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
