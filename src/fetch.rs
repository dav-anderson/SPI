//! spi's release source: GitHub releases for psp-mint and esp-mint (the shared
//! [`Github`] source in air-minterface) and bitcoincore.org for bitcoind, through
//! `curl`, run through the same [`Exec`] as every other program (no HTTP crate).
//!
//! - psp-mint and esp-mint: the GitHub API lists a repository's releases and
//!   their assets. The archive is `<program>-<version>-<os>-<arch>.tar.gz`
//!   (`os` is `linux` or `macos`, `arch` is `x86_64` or `aarch64`) and the
//!   release carries a `SHA256SUMS` asset. All of that is air-minterface's.
//! - bitcoind: bitcoincore.org keeps one folder per release, `bin/bitcoin-core-<version>/`,
//!   with `bitcoin-<version>-<cpu>-<os>.tar.gz` and `SHA256SUMS`. This is spi's.
//!
//! The archive's SHA-256 is checked against `SHA256SUMS` before anything is
//! unpacked. For bitcoind the signatures on `SHA256SUMS` are checked first
//! (see [`crate::signature`]): a checksum served beside the file does not stop
//! a hijacked download, the builders' signatures do. A release candidate is
//! never offered: a folder with no `SHA256SUMS` is skipped, and a tag with
//! anything but numbers in it is not a version.

use std::fs;
use std::path::{Path, PathBuf};

use air_minterface::Managed;
use air_minterface::exec::Exec;
use air_minterface::github::{Archive, Github};
use air_minterface::release::{self, Download, Source};
use air_minterface::version::{Tags, same_version};

use crate::config::Spi;
use crate::signature;
use crate::upgrade::Program;

/// GitHub releases and bitcoincore.org, through `curl`.
pub struct CurlSource<'a> {
    github: Github<'a>,
    exec: &'a dyn Exec,
    bitcoin: String,
    /// Bitcoin Core's signatures: the private keyring, where the builders' keys
    /// are, who is trusted and how many must have signed.
    gnupg: PathBuf,
    keys_url: String,
    signers: Vec<(String, String)>,
    min_signatures: u64,
}

impl<'a> CurlSource<'a> {
    /// The source for this machine.
    pub fn new(spi: &Spi, exec: &'a dyn Exec) -> CurlSource<'a> {
        let os = if spi.mac { "macos" } else { "linux" };
        CurlSource::for_machine(spi, exec, os, std::env::consts::ARCH)
    }

    /// The source for a machine of `os` and `arch` (the tests name one).
    pub fn for_machine(
        spi: &Spi,
        exec: &'a dyn Exec,
        os: &'static str,
        arch: &str,
    ) -> CurlSource<'a> {
        CurlSource {
            github: Github::for_machine(spi, exec, os, arch),
            exec,
            bitcoin: spi.release.bitcoin_url.clone(),
            gnupg: spi.dir.join("gnupg"),
            keys_url: spi.release.builder_keys_url.clone(),
            signers: spi.bitcoind_signers.clone().unwrap_or_else(|| {
                signature::BUILDERS
                    .iter()
                    .map(|(f, n)| (f.to_string(), n.to_string()))
                    .collect()
            }),
            min_signatures: spi.bitcoind_min_signatures,
        }
    }

    /// Check the signatures on Bitcoin Core's `SHA256SUMS` (`sums`, fetched from
    /// `sums_url`) before anything in it is trusted. Work files go in `dir`.
    fn verify_sums(
        &self,
        version: &str,
        sums: &str,
        sums_url: &str,
        dir: &Path,
    ) -> Result<(), String> {
        let work = dir.join("signatures");
        fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
        let (sums_file, asc) = (work.join("SHA256SUMS"), work.join("SHA256SUMS.asc"));
        let get = |url: &str, to: &Path| {
            self.github
                .curl(&["-o", &to.display().to_string(), url])
                .map(|_| ())
        };
        let result = fs::write(&sums_file, sums)
            .map_err(|e| format!("cannot write {}: {e}", sums_file.display()))
            .and_then(|()| get(&format!("{sums_url}.asc"), &asc))
            .and_then(|()| {
                let check = signature::Check {
                    exec: self.exec,
                    home: &self.gnupg,
                    keys_url: &self.keys_url,
                    signers: &self.signers,
                    min: self.min_signatures,
                };
                signature::verify(&check, version, (&sums_file, &asc, &work), &get)
            });
        let _ = fs::remove_dir_all(&work);
        if result.is_err() {
            let _ = fs::remove_dir(dir);
        }
        result
    }

    /// The bitcoind archive for this machine: its name, where it is, and where
    /// the release's `SHA256SUMS` is.
    fn bitcoind_archive(&self, version: &str) -> Result<Archive, String> {
        let cpu = match (self.github.os, self.github.arch.as_str()) {
            ("linux", "x86_64") => "x86_64-linux-gnu",
            ("linux", "aarch64") => "aarch64-linux-gnu",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "arm64-apple-darwin",
            (os, arch) => return Err(format!("Bitcoin Core has no build for {os} on {arch}")),
        };
        let name = format!("bitcoin-{version}-{cpu}.tar.gz");
        let base = format!("{}/bin/bitcoin-core-{version}", self.bitcoin);
        Ok(Archive {
            url: format!("{base}/{name}"),
            sums_url: format!("{base}/SHA256SUMS"),
            name,
        })
    }
}

/// The versions bitcoincore.org lists: every `bitcoin-core-<digits and dots>/`.
fn bitcoin_versions(index: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in index.split("bitcoin-core-").skip(1) {
        let version: String = part
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if part[version.len()..].starts_with('/') && !out.contains(&version) {
            out.push(version);
        }
    }
    out
}

impl Source<Program> for CurlSource<'_> {
    fn tags(&self, program: Program) -> Result<Vec<String>, String> {
        match program {
            Program::Air => Err("air_server publishes no releases".into()),
            Program::Bitcoind => {
                let index = self.github.curl(&[&format!("{}/bin/", self.bitcoin)])?;
                let mut all = bitcoin_versions(&index);
                // A folder can exist before its release does (32.0 held only
                // `test.rc*` folders): drop the newest until one has SHA256SUMS.
                while let Some(newest) = Tags::Loose.newest(&all) {
                    let sums = format!("{}/bin/bitcoin-core-{newest}/SHA256SUMS", self.bitcoin);
                    if self.github.curl(&[&sums]).is_ok() {
                        break;
                    }
                    all.retain(|v| *v != newest);
                }
                Ok(all)
            }
            Program::Psp | Program::Esp => self.github.tags_of(program.name()),
        }
    }

    fn fetch(&self, program: Program, version: &str, dir: &Path) -> Result<Download, String> {
        if program != Program::Bitcoind {
            return self.github.fetch_release(program.name(), version, dir);
        }
        let archive = self.bitcoind_archive(version)?;
        self.github
            .fetch_with(program.name(), &archive, dir, &|sums, sums_url| {
                self.verify_sums(version, sums, sums_url, dir)
            })
    }
}

/// What `spi install` does about downloads: say whether curl is there, and with
/// `[bitcoind] manage = true` and no `bin` written, fetch bitcoind (the pin,
/// else the newest release) into `releases/` and return a `Spi` whose `bin` is
/// that file, so the service files install it. The lines are for the operator.
pub fn prepare_install(
    spi: &Spi,
    exec: &dyn Exec,
    source: &dyn Source<Program>,
) -> Result<(Spi, Vec<String>), String> {
    let mut lines = Vec::new();
    if !exec
        .run("curl", &["--version".to_string()], &[])
        .is_ok_and(|r| r.ok())
    {
        lines.push("NOTE: curl is not installed. spi uses it to download releases (bitcoind, and `spi upgrade`); install curl, or supply the builds yourself with `bin`".into());
    }
    if !spi.manage_bitcoind || spi.bitcoind_bin_set {
        return Ok((spi.clone(), lines));
    }
    let version = match Program::Bitcoind.pin(spi) {
        Some(pin) => pin,
        None => release::newest_of(source, Program::Bitcoind)
            .map_err(|e| format!("cannot find the newest Bitcoin Core: {e} (set `version` under [bitcoind] to use a download you already have, or `bin` for your own bitcoind)"))?
            .ok_or("bitcoincore.org lists no Bitcoin Core release")?,
    };
    let dir = release::releases_dir(spi, Program::Bitcoind, &version);
    let file = dir.join("bitcoind");
    if !file.is_file() {
        signature::require_gpg(exec)?;
        source.fetch(Program::Bitcoind, &version, &dir)?;
    }
    let found = Program::Bitcoind.reported(spi, exec, &file)?;
    if !same_version(&found, &version) {
        return Err(format!("{} is {found}, not {version}", file.display()));
    }
    lines.push(format!(
        "bitcoind {version} is in {} (downloaded from bitcoincore.org and checked against its SHA256SUMS and its builders' signatures)",
        file.display()
    ));
    let mut spi = spi.clone();
    spi.bitcoind_bin = file.display().to_string();
    Ok((spi, lines))
}
