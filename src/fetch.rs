//! The real release source: releases are listed and downloaded with `curl`,
//! run through the same [`Exec`] as every other program (no HTTP crate).
//!
//! - psp-mint and esp-mint: the GitHub API lists a repository's releases and
//!   their assets. The archive is `<program>-<version>-<os>-<arch>.tar.gz`
//!   (`os` is `linux` or `macos`, `arch` is `x86_64` or `aarch64`) and the
//!   release carries a `SHA256SUMS` asset.
//! - bitcoind: bitcoincore.org keeps one folder per release, `bin/bitcoin-core-<version>/`,
//!   with `bitcoin-<version>-<cpu>-<os>.tar.gz` and `SHA256SUMS`.
//!
//! The archive's SHA-256 is checked against `SHA256SUMS` here, before anything
//! is unpacked. For bitcoind the signatures on `SHA256SUMS` are checked first
//! (see [`crate::signature`]): a checksum served beside the file does not stop
//! a hijacked download, the builders' signatures do. A release candidate is
//! never offered: a folder with no `SHA256SUMS` is skipped, and a tag with
//! anything but numbers in it is not a version.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::Spi;
use crate::exec::Exec;
use crate::release::{self, Download, Source};
use crate::signature;
use crate::upgrade::{self, Program};
use crate::versions;

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct Rel {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// GitHub releases and bitcoincore.org, through `curl`.
pub struct CurlSource<'a> {
    exec: &'a dyn Exec,
    api: String,
    bitcoin: String,
    os: &'static str,
    arch: String,
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
            exec,
            api: spi.release.url.clone(),
            bitcoin: spi.release.bitcoin_url.clone(),
            os,
            arch: arch.to_string(),
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
            self.curl(&["-o", &to.display().to_string(), url])
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

    fn curl(&self, args: &[&str]) -> Result<String, String> {
        let mut all: Vec<String> = ["-fsSL", "--max-time", "900"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        all.extend(args.iter().map(|s| s.to_string()));
        let ran = self.exec.run("curl", &all, &[]).map_err(|e| {
            format!("{e} (spi downloads releases with curl: install curl, or put the build in place and name it with `bin`)")
        })?;
        if ran.ok() {
            Ok(ran.stdout)
        } else {
            Err(format!(
                "curl {}: {}",
                args.last().unwrap_or(&""),
                ran.last_line()
            ))
        }
    }

    fn releases(&self, program: Program) -> Result<Vec<Rel>, String> {
        let url = format!(
            "{}/repos/dav-anderson/{}/releases?per_page=100",
            self.api,
            program.name()
        );
        let body = self.curl(&["-H", "Accept: application/vnd.github+json", &url])?;
        let list: Vec<Rel> = serde_json::from_str(&body)
            .map_err(|e| format!("the release list at {url} is not what GitHub sends: {e}"))?;
        Ok(list.into_iter().filter(|r| !r.draft).collect())
    }

    /// The archive for this machine: its name, where it is, and where the
    /// release's `SHA256SUMS` is.
    fn archive(&self, program: Program, version: &str) -> Result<(String, String, String), String> {
        if program == Program::Bitcoind {
            let cpu = match (self.os, self.arch.as_str()) {
                ("linux", "x86_64") => "x86_64-linux-gnu",
                ("linux", "aarch64") => "aarch64-linux-gnu",
                ("macos", "x86_64") => "x86_64-apple-darwin",
                ("macos", "aarch64") => "arm64-apple-darwin",
                (os, arch) => return Err(format!("Bitcoin Core has no build for {os} on {arch}")),
            };
            let name = format!("bitcoin-{version}-{cpu}.tar.gz");
            let base = format!("{}/bin/bitcoin-core-{version}", self.bitcoin);
            return Ok((
                name.clone(),
                format!("{base}/{name}"),
                format!("{base}/SHA256SUMS"),
            ));
        }
        let list = self.releases(program)?;
        let Some(rel) = list
            .into_iter()
            .find(|r| r.tag_name.strip_prefix('v').unwrap_or(&r.tag_name) == version)
        else {
            return Err(format!("{} has no release {version}", program.name()));
        };
        if rel.prerelease {
            return Err(format!(
                "{} {version} is marked as a pre-release on GitHub; spi installs releases only",
                program.name()
            ));
        }
        let name = format!(
            "{}-{version}-{}-{}.tar.gz",
            program.name(),
            self.os,
            self.arch
        );
        let find = |wanted: &str| {
            rel.assets
                .iter()
                .find(|a| a.name == wanted)
                .map(|a| a.browser_download_url.clone())
        };
        let Some(url) = find(&name) else {
            let have: Vec<&str> = rel.assets.iter().map(|a| a.name.as_str()).collect();
            return Err(format!(
                "release {} of {} has no archive for this machine ({} on {}): expected {name}, it has {}",
                rel.tag_name,
                program.name(),
                self.os,
                self.arch,
                have.join(", ")
            ));
        };
        let Some(sums) = find("SHA256SUMS") else {
            return Err(format!(
                "release {} of {} has no SHA256SUMS",
                rel.tag_name,
                program.name()
            ));
        };
        Ok((name, url, sums))
    }
}

/// The SHA-256 `SHA256SUMS` (`<hex>  <name>` lines) gives for `name`.
fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hex, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hex.to_string())
    })
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

/// The first file called `name` under `dir`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
    }
    None
}

impl Source for CurlSource<'_> {
    fn tags(&self, program: Program) -> Result<Vec<String>, String> {
        match program {
            Program::Air => Err("air_server publishes no releases".into()),
            Program::Bitcoind => {
                let index = self.curl(&[&format!("{}/bin/", self.bitcoin)])?;
                let mut all = bitcoin_versions(&index);
                // A folder can exist before its release does (32.0 held only
                // `test.rc*` folders): drop the newest until one has SHA256SUMS.
                while let Some(newest) = release::newest(&all) {
                    let sums = format!("{}/bin/bitcoin-core-{newest}/SHA256SUMS", self.bitcoin);
                    if self.curl(&[&sums]).is_ok() {
                        break;
                    }
                    all.retain(|v| *v != newest);
                }
                Ok(all)
            }
            Program::Psp | Program::Esp => Ok(self
                .releases(program)?
                .into_iter()
                .filter(|r| !r.prerelease)
                .map(|r| r.tag_name)
                .collect()),
        }
    }

    fn fetch(&self, program: Program, version: &str, dir: &Path) -> Result<Download, String> {
        let (name, url, sums_url) = self.archive(program, version)?;
        let sums = self.curl(&[&sums_url])?;
        if program == Program::Bitcoind {
            self.verify_sums(version, &sums, &sums_url, dir)?;
        }
        let Some(published) = sum_for(&sums, &name) else {
            return Err(format!("{sums_url} does not list {name}"));
        };
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let archive = dir.join(&name);
        let unpacked = dir.join("unpacked");
        let result = (|| {
            self.curl(&["-o", &archive.display().to_string(), &url])?;
            let have = versions::sha256_file(&archive)?;
            if !have.eq_ignore_ascii_case(&published) {
                return Err(format!(
                    "the download of {name} failed its checksum (SHA256SUMS says {published}, the file is {have}): it was deleted and nothing was changed"
                ));
            }
            let args = [
                "-xzf".to_string(),
                archive.display().to_string(),
                "-C".into(),
                unpacked.display().to_string(),
            ];
            fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
            let ran = self.exec.run("tar", &args, &[])?;
            if !ran.ok() {
                return Err(format!("cannot unpack {name}: {}", ran.last_line()));
            }
            let found = find_file(&unpacked, program.name())
                .ok_or_else(|| format!("{name} holds no file called {}", program.name()))?;
            let file = dir.join(program.name());
            fs::rename(&found, &file)
                .map_err(|e| format!("cannot move {}: {e}", found.display()))?;
            release::make_executable(&file)?;
            Ok(Download {
                sha256: versions::sha256_file(&file)?,
                file,
            })
        })();
        let _ = fs::remove_file(&archive);
        let _ = fs::remove_dir_all(&unpacked);
        result
    }
}

/// What `spi install` does about downloads: say whether curl is there, and with
/// `[bitcoind] manage = true` and no `bin` written, fetch bitcoind (the pin,
/// else the newest release) into `releases/` and return a `Spi` whose `bin` is
/// that file, so the service files install it. The lines are for the operator.
pub fn prepare_install(
    spi: &Spi,
    exec: &dyn Exec,
    source: &dyn Source,
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
        Some(pin) => pin.to_string(),
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
    let found = upgrade::reported(spi, exec, Program::Bitcoind, &file)?;
    if !versions::same_version(&found, &version) {
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
