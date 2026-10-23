//! `spi versions`: the version of every program and library the SPI runs.
//!
//! - **spi** and the libraries it was built with: from the build (`Cargo.lock`).
//! - **psp-mint, esp-mint, ecash-wallet**: `<bin> --version`, whose first line
//!   is the program and whose other lines are the libraries it was built with
//!   (sp-wire, mint-core, air), shown as `psp-mint/sp-wire` and so on. The
//!   library versions are exact when the program was built with `--locked`.
//! - **bitcoind**: `bitcoind --version`.
//! - **air_server** prints no version. spi therefore keeps its own record: when
//!   `spi install` or `spi upgrade` puts a binary in place it writes the
//!   version `[air] version` names and the SHA-256 of the binary to
//!   `<spi folder>/air/installed.json`. A binary whose hash matches the record
//!   has that version; one that does not is `unknown`; with no record the line
//!   says the version is only the one named in spi.toml and was not checked.
//!
//! Every line is a version or `unknown` with the reason. Nothing is started,
//! stopped or written.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Spi;
use crate::exec::Exec;
use crate::service::find_binary;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Line {
    pub name: String,
    /// `x.y.z`, or `unknown`.
    pub version: String,
    /// Where the version came from, or why it is unknown.
    pub note: String,
}

fn known(name: &str, version: &str, note: &str) -> Line {
    Line {
        name: name.into(),
        version: version.into(),
        note: note.into(),
    }
}

fn unknown(name: &str, why: impl Into<String>) -> Line {
    known(name, "unknown", &why.into())
}

/// The SHA-256 of a file, as hex.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// What spi remembers of the `air_server` it installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub version: String,
    pub sha256: String,
}

pub fn air_record_path(spi: &Spi) -> PathBuf {
    spi.air_dir().join("installed.json")
}

/// Write the record: `binary` is the `air_server` that is being put in place
/// and `version` what it is (air_server cannot say).
pub fn record_air(spi: &Spi, binary: &Path, version: &str) -> Result<(), String> {
    let record = Installed {
        version: version.to_string(),
        sha256: sha256_file(binary)?,
    };
    let path = air_record_path(spi);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn read_record(spi: &Spi) -> Option<Installed> {
    serde_json::from_slice(&fs::read(air_record_path(spi)).ok()?).ok()
}

/// The version spi recorded for exactly this `air_server` binary, if it did.
pub fn air_recorded(spi: &Spi, binary: &Path) -> Option<String> {
    let record = read_record(spi)?;
    (sha256_file(binary).ok()? == record.sha256).then_some(record.version)
}

/// The version of the `air_server` at `binary`.
pub fn air_line(spi: &Spi, label: &str, binary: &Path) -> Line {
    let sha = match sha256_file(binary) {
        Ok(s) => s,
        Err(e) => return unknown(label, e),
    };
    match (read_record(spi), &spi.air.version) {
        (Some(r), _) if r.sha256 == sha => known(
            label,
            &r.version,
            "recorded by spi for this exact binary (air_server prints no version)",
        ),
        (Some(r), _) => unknown(
            label,
            format!(
                "{} is not the binary spi recorded as {} (its SHA-256 differs), and air_server prints no version",
                binary.display(),
                r.version
            ),
        ),
        (None, Some(v)) => known(
            label,
            v,
            "named in spi.toml, not checked: spi has no record of this binary and air_server prints no version (`spi install` records it)",
        ),
        (None, None) => unknown(
            label,
            "air_server prints no version, spi has no record of installing it and [air] version is not set",
        ),
    }
}

/// The program's own version from the first line of its `--version`:
/// `<name> <version>`.
pub fn program_version(name: &str, first_line: &str) -> Option<String> {
    let mut words = first_line.split_whitespace();
    (words.next()? == name).then(|| words.next().map(str::to_string))?
}

/// bitcoind's version from the first line of its `--version`: the word after
/// `version`, without the leading `v` (`Bitcoin Core version v29.1.0`).
pub fn bitcoind_version(first_line: &str) -> Option<String> {
    let mut words = first_line.split_whitespace();
    words.find(|w| *w == "version")?;
    let v = words.next()?.trim_start_matches('v');
    (!v.is_empty() && v.starts_with(|c: char| c.is_ascii_digit())).then(|| v.to_string())
}

/// Run `<bin> --version` and turn the answer into lines: the program, then each
/// library it was built with.
fn program_lines(exec: &dyn Exec, name: &str, label: &str, bin: &str) -> Vec<Line> {
    let ran = match exec.run(bin, &["--version".to_string()], &[]) {
        Ok(r) => r,
        Err(e) => return vec![unknown(label, e)],
    };
    if !ran.ok() {
        return vec![unknown(
            label,
            format!(
                "`{bin} --version` failed ({}): this build may be older than --version",
                ran.last_line()
            ),
        )];
    }
    let mut lines = ran.stdout.lines();
    let first = lines.next().unwrap_or("");
    let Some(version) = program_version(name, first) else {
        return vec![unknown(
            label,
            format!("`{bin} --version` printed `{first}`, not `{name} <version>`"),
        )];
    };
    let mut out = vec![known(label, &version, &format!("`{bin} --version`"))];
    for lib in lines {
        let mut words = lib.split_whitespace();
        if let (Some(n), Some(v)) = (words.next(), words.next()) {
            out.push(known(
                &format!("{label}/{n}"),
                v,
                "built in (from the program's Cargo.lock)",
            ));
        }
    }
    out
}

/// Every program and library, in order.
pub fn versions(spi: &Spi, exec: &dyn Exec) -> Vec<Line> {
    let mut out = vec![known("spi", env!("CARGO_PKG_VERSION"), "this program")];
    for lib in env!("BUILT_WITH").split(';').filter(|s| !s.is_empty()) {
        let mut words = lib.split_whitespace();
        if let (Some(n), Some(v)) = (words.next(), words.next()) {
            out.push(known(
                &format!("spi/{n}"),
                v,
                "built in (from spi's Cargo.lock)",
            ));
        }
    }
    let installed = |name: &str| PathBuf::from(&spi.service.bin_dir).join(name);
    for (name, bin) in [
        ("psp-mint", spi.psp.bin.as_str()),
        ("esp-mint", spi.esp.bin.as_str()),
        ("ecash-wallet", spi.wallet_settings.bin.as_str()),
    ] {
        out.extend(program_lines(exec, name, name, bin));
        let copy = installed(name);
        if copy.is_file() {
            out.extend(program_lines(
                exec,
                name,
                &format!("{name} (service)"),
                &copy.display().to_string(),
            ));
        }
    }
    let bitcoind = |label: &str, bin: &str| -> Line {
        match exec.run(bin, &["--version".to_string()], &[]) {
            Err(e) => unknown(label, e),
            Ok(r) if !r.ok() => unknown(
                label,
                format!("`{bin} --version` failed: {}", r.last_line()),
            ),
            Ok(r) => {
                let first = r.stdout.lines().next().unwrap_or("");
                match bitcoind_version(first) {
                    Some(v) => known(label, &v, &format!("`{bin} --version`")),
                    None => unknown(
                        label,
                        format!("`{bin} --version` printed `{first}`, which has no version"),
                    ),
                }
            }
        }
    };
    out.push(bitcoind("bitcoind", &spi.bitcoind_bin));
    let copy = installed("bitcoind");
    if copy.is_file() {
        out.push(bitcoind("bitcoind (service)", &copy.display().to_string()));
    }
    match find_binary(&spi.air.bin) {
        Some(p) => out.push(air_line(spi, "air_server", &p)),
        None => out.push(unknown(
            "air_server",
            format!(
                "cannot find `{}` (air.bin); air_server prints no version",
                spi.air.bin
            ),
        )),
    }
    let copy = installed("air_server");
    if copy.is_file() {
        out.push(air_line(spi, "air_server (service)", &copy));
    }
    out
}

/// Text for a person.
pub fn render(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(&format!("{:<32} {:<10} {}\n", l.name, l.version, l.note));
    }
    out
}

/// Whether two versions are the same number: `29.1` and `29.1.0` are.
pub fn same_version(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u64>> {
        let mut n: Vec<u64> = v
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<Vec<_>>>()?;
        while n.len() < 3 {
            n.push(0);
        }
        Some(n)
    };
    match (parts(a), parts(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}
