//! The real release source, `curl` and `tar` included, against a stub server:
//! the archive for the machine, a checksum that fails, a release with no
//! archive for the machine, and bitcoind from bitcoincore.org.

mod common;

use std::sync::{Arc, Mutex};

use common::{LINUX_ARCHIVE, MAC_ARCHIVE, Seen, github, load, scratch, sha, stub_bytes, tarball};
use spi::exec::System;
use spi::fetch::CurlSource;
use spi::release::Source;
use spi::upgrade::Program;

/// The source for a machine of `os` and `arch`, with GitHub and bitcoincore.org
/// both at `addr`.
fn curl_for(addr: &str, os: &'static str, arch: &str) -> CurlSource<'static> {
    let text = format!("[release]\nurl = \"http://{addr}\"\nbitcoin_url = \"http://{addr}\"\n");
    let spi = load(&scratch("config"), Some(text.as_str())).unwrap();
    CurlSource::for_machine(&spi, &System, os, arch)
}

/// The Linux source for bitcoind, whose gpg says `status` about the signatures.
fn curl_gpg(addr: &str, status: String) -> CurlSource<'static> {
    let exec: &'static common::GpgSays = Box::leak(Box::new(common::GpgSays(status)));
    let text = format!(
        "[release]\nurl = \"http://{addr}\"\nbitcoin_url = \"http://{addr}\"\nbuilder_keys_url = \"http://{addr}/keys\"\n"
    );
    let spi = load(&scratch("config"), Some(text.as_str())).unwrap();
    CurlSource::for_machine(&spi, exec, "linux", "x86_64")
}

#[test]
fn the_archive_for_this_machine_is_the_one_downloaded_and_unpacked() {
    let (addr, seen) = github(false);
    let dir = scratch("mac").join("0.6.0");
    let curl = curl_for(&addr, "macos", "aarch64");
    let got = curl.fetch(Program::Psp, "0.6.0", &dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(got.file).unwrap(),
        "psp-mint 0.6.0\n"
    );
    let asked = |name: &str| seen.lock().unwrap().iter().any(|s| s.path.ends_with(name));
    assert!(asked(MAC_ARCHIVE) && !asked(LINUX_ARCHIVE));
}

#[test]
fn a_download_that_fails_its_checksum_is_deleted_and_nothing_is_left() {
    let (addr, _) = github(true);
    let dir = scratch("bad").join("0.6.0");
    let curl = curl_for(&addr, "linux", "x86_64");
    let err = curl.fetch(Program::Psp, "0.6.0", &dir).unwrap_err();
    assert!(
        err.contains("failed its checksum") && err.contains(&"0".repeat(64)),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn a_release_with_no_archive_for_this_machine_is_refused_and_says_so() {
    let (addr, _) = github(false);
    let dir = scratch("none").join("0.6.0");
    let curl = curl_for(&addr, "linux", "riscv64");
    let err = curl.fetch(Program::Psp, "0.6.0", &dir).unwrap_err();
    assert!(
        err.contains("has no archive for this machine (linux on riscv64)")
            && err.contains(LINUX_ARCHIVE),
        "{err}"
    );
    assert!(!dir.exists());
}

// ---- bitcoind from bitcoincore.org ----

const BITCOIN_ARCHIVE: &str = "bitcoin-30.0-x86_64-linux-gnu.tar.gz";

/// A stub bitcoincore.org with 29.2, 30.0, a candidate (30.1rc1, which has
/// a SHA256SUMS) and a 31.0 folder that holds no release yet; 30.0 has a Linux
/// archive. With `lie` its SHA256SUMS gives the archive a wrong checksum.
fn bitcoincore(lie: bool) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let archive = tarball("bitcoin-30.0/bin/bitcoind", "bitcoind 30.0\n");
    let sum = if lie { "f".repeat(64) } else { sha(&archive) };
    stub_bytes(move |_, path, _| {
        match path {
        "/bin/" => (200, b"<a href=\"bitcoin-core-29.2/\">29.2</a> <a href=\"bitcoin-core-30.0/\">30.0</a> <a href=\"bitcoin-core-30.1rc1/\">rc</a> <a href=\"bitcoin-core-31.0/\">unreleased</a>".to_vec()),
        "/bin/bitcoin-core-30.1rc1/SHA256SUMS" => (200, Vec::new()),
        "/bin/bitcoin-core-30.0/SHA256SUMS.asc" => (200, b"signatures".to_vec()),
        "/bin/bitcoin-core-30.0/SHA256SUMS" => (200, format!("{sum}  {BITCOIN_ARCHIVE}\n").into_bytes()),
        p if p == format!("/bin/bitcoin-core-30.0/{BITCOIN_ARCHIVE}") => (200, archive.clone()),
        _ => (404, Vec::new()),
    }
    })
}

#[test]
fn bitcoind_is_listed_downloaded_checked_and_unpacked() {
    let (addr, _) = bitcoincore(false);
    let curl = curl_gpg(&addr, common::signed_by(7));
    let newest = spi::release::newest_of(&curl, Program::Bitcoind).unwrap();
    assert_eq!(newest.as_deref(), Some("30.0"));
    let dir = scratch("bitcoind").join("30.0");
    let got = curl.fetch(Program::Bitcoind, "30.0", &dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(got.file).unwrap(),
        "bitcoind 30.0\n"
    );
}

#[test]
fn a_bitcoind_that_fails_its_sha256sums_is_refused() {
    let (addr, _) = bitcoincore(true);
    let dir = scratch("bitcoind-bad").join("30.0");
    let curl = curl_gpg(&addr, common::signed_by(7));
    let err = curl.fetch(Program::Bitcoind, "30.0", &dir).unwrap_err();
    assert!(err.contains("failed its checksum"), "{err}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn a_bitcoind_signed_by_too_few_builders_is_refused_before_its_archive_is_fetched() {
    let (addr, seen) = bitcoincore(false);
    let dir = scratch("bitcoind-unsigned").join("30.0");
    let curl = curl_gpg(&addr, common::signed_by(6));
    let err = curl.fetch(Program::Bitcoind, "30.0", &dir).unwrap_err();
    assert!(
        err.contains("6 good signatures from trusted builders, 7 needed"),
        "{err}"
    );
    let asked = seen
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.path.ends_with(BITCOIN_ARCHIVE));
    assert!(!dir.exists() && !asked);
}
