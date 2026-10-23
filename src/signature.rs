//! The signature check of Bitcoin Core's `SHA256SUMS`, with `gpg`.
//!
//! A checksum served beside the file does not stop a hijacked download; the
//! signatures on the checksum file do. Bitcoin Core's `SHA256SUMS.asc` holds one
//! detached signature for each builder who reproduced the release. spi:
//!
//! 1. downloads the builders' public keys (`builder-keys/<name>.gpg` of
//!    `bitcoin-core/guix.sigs`) into a private keyring, `<spi folder>/gnupg`;
//! 2. runs `gpg --verify SHA256SUMS.asc SHA256SUMS` and reads gpg's status
//!    lines;
//! 3. counts the good signatures whose **primary key fingerprint** (the last
//!    field of `VALIDSIG`) is on the trusted list, each fingerprint once.
//!
//! Only the fingerprints in [`BUILDERS`] (or `[bitcoind] signers`) count, so a
//! key file from a wrong place cannot add a signer. Any `BADSIG` fails the
//! download. A signature by a key spi does not have (`ERRSIG`, `NO_PUBKEY`) or
//! by a revoked key (`REVKEYSIG`) is not counted and is not a failure. A
//! signature by a key that has since expired (`EXPKEYSIG`) is counted: real
//! releases carry them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::exec::Exec;

/// How many trusted builders must have signed (`[bitcoind] min_signatures`).
pub const DEFAULT_MIN_SIGNATURES: u64 = 7;

/// The builders spi trusts: primary key fingerprint, and the name of the key
/// file in `guix.sigs/builder-keys`. Every builder who signed Bitcoin Core
/// 31.1, 31.0, 30.3 or 29.2, checked by the PM on 2026-10-23.
pub const BUILDERS: [(&str, &str); 18] = [
    ("0AD83877C1F0CD1EE9BD660AD7CC770B81FD22A8", "benthecarman"),
    ("152812300785C96444D3334D17565732E08E5E41", "achow101"),
    ("33C103B4B2794170546CCF7BCFB2C83C66CD792A", "svanstaa"),
    ("5B286407E1EA6FE01CF9AF48BF131C2D0536F8AC", "marcofleon"),
    ("67AA5B46E7AF78053167FE343B8F814A784218F8", "willcl-ark"),
    ("6A8F9C266528E25AEB1D7731C2371D91CB716EA7", "theStack"),
    ("6B002C6EA3F91B1B0DF0C9BC8F617F1200A6D25C", "glozow"),
    ("71A3B16735405025D447E8F274810B012346C9A6", "laanwj"),
    ("982A193E3CE0EED535E09023188CBB2648416AD5", "0xb10c"),
    ("9D3CC86A72F8494342EA5FD10A41BDC3F4FAFF1C", "sipsorcery"),
    ("9EDAFF80E080659604F4A76B2EBB056FD847F8A7", "Emzy"),
    ("A8FC55F3B04BA3146F3492E79303B33A305224CB", "TheCharlatan"),
    ("D1DBF2C4B96F2DEBF4C16654410108112E7EA81F", "hebasto"),
    ("E61773CD6E01040E2F1BD78CE7E2984B6289C93A", "pinheadmz"),
    ("E777299FC265DD04793070EB944D35F9AC3DB76A", "fanquake"),
    ("E86AE73439625BBEE306AAE6B66D427F873CB1A3", "m3dwards"),
    ("ED9BDF7AD6A55E232E84524257FF9BDBCC301009", "Sjors"),
    ("F4FC70F07310028424EFC20A8E4256593F177720", "guggero"),
];

const NO_GPG: &str = "gpg is not installed. spi checks the signatures on Bitcoin Core's SHA256SUMS with it, and will not download bitcoind without that check. Install it (Debian and Ubuntu: `sudo apt install gnupg`; macOS: `brew install gnupg`), or supply your own bitcoind with `[bitcoind] bin`, or let spi leave bitcoind alone with `manage = false`";

/// `Ok` if gpg runs; else `Err` saying how to get it.
pub fn require_gpg(exec: &dyn Exec) -> Result<(), String> {
    let ran = exec.run("gpg", &["--version".to_string()], &[]);
    ran.is_ok_and(|r| r.ok())
        .then_some(())
        .ok_or_else(|| NO_GPG.to_string())
}

/// A `[bitcoind] signers` entry: `<40 hex digits> <builder-keys file name>`.
pub fn parse_signer(text: &str) -> Option<(String, String)> {
    let mut words = text.split_whitespace();
    let (fingerprint, name) = (words.next()?, words.next()?);
    let name_ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    (words.next().is_none()
        && fingerprint.len() == 40
        && fingerprint.chars().all(|c| c.is_ascii_hexdigit())
        && name_ok)
        .then(|| (fingerprint.to_ascii_uppercase(), name.to_string()))
}

/// The names of the trusted builders with a good signature in gpg's
/// `--status-fd` output, each once. `Err` if any signature is bad.
pub fn count(status: &str, signers: &[(String, String)]) -> Result<Vec<String>, String> {
    let (mut found, mut good): (Vec<&(String, String)>, bool) = (Vec::new(), false);
    for line in status.lines() {
        let mut words = line.strip_prefix("[GNUPG:] ").unwrap_or("").split(' ');
        match words.next() {
            Some("BADSIG") => return Err(format!("gpg reports a BAD signature: {}", line.trim())),
            Some("GOODSIG" | "EXPKEYSIG") => good = true,
            Some("NEWSIG" | "REVKEYSIG" | "EXPSIG" | "ERRSIG") => good = false,
            Some("VALIDSIG") => {
                let primary = words.next_back().unwrap_or("");
                let signer = signers.iter().find(|s| s.0.eq_ignore_ascii_case(primary));
                if let (true, Some(s)) = (std::mem::take(&mut good), signer)
                    && !found.contains(&s)
                {
                    found.push(s);
                }
            }
            _ => {}
        }
    }
    Ok(found.into_iter().map(|s| s.1.clone()).collect())
}

/// What the check needs.
pub struct Check<'a> {
    pub exec: &'a dyn Exec,
    /// The private keyring.
    pub home: &'a Path,
    /// Where `<name>.gpg` of each signer is.
    pub keys_url: &'a str,
    pub signers: &'a [(String, String)],
    pub min: u64,
}

/// Check the signatures `asc` makes on `sums` for Bitcoin Core `release`. `get`
/// downloads a URL to a file; key files go to `work`. `Ok` only when at least
/// `min` trusted builders signed and no signature is bad.
pub fn verify(
    c: &Check,
    release: &str,
    (sums, asc, work): (&Path, &Path, &Path),
    get: &dyn Fn(&str, &Path) -> Result<(), String>,
) -> Result<(), String> {
    let text = |p: &Path| p.display().to_string();
    let home = text(c.home);
    require_gpg(c.exec)?;
    fs::create_dir_all(c.home).map_err(|e| format!("cannot create {}: {e}", c.home.display()))?;
    fs::set_permissions(c.home, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot protect {}: {e}", c.home.display()))?;
    let gpg = |args: Vec<String>| {
        let mut all: Vec<String> = ["--homedir", &home, "--batch", "--no-tty"]
            .map(String::from)
            .into();
        all.extend(args);
        c.exec.run("gpg", &all, &[])
    };
    let (mut import, mut missing) = (vec!["--import".to_string()], Vec::new());
    for (_, name) in c.signers {
        let file = work.join(format!("{name}.gpg"));
        match get(&format!("{}/{name}.gpg", c.keys_url), &file) {
            Ok(()) => import.push(text(&file)),
            Err(_) => missing.push(name.as_str()),
        }
    }
    gpg(import)?;
    let (asc, sums) = (text(asc), text(sums));
    let args = ["--status-fd", "1", "--verify", &asc, &sums].map(String::from);
    let ran = gpg(args.into())?;
    let signed = count(&ran.stdout, c.signers)
        .map_err(|e| format!("bitcoin core {release}: {e}. Nothing was installed."))?;
    if (signed.len() as u64) < c.min {
        let who = if signed.is_empty() {
            "no one".to_string()
        } else {
            signed.join(", ")
        };
        let keys = if missing.is_empty() {
            String::new()
        } else {
            format!(
                " The keys of {} could not be downloaded.",
                missing.join(", ")
            )
        };
        return Err(format!(
            "bitcoin core {release}: {} good signatures from trusted builders, {} needed (signed by: {who}).{keys} Nothing was installed.",
            signed.len(),
            c.min
        ));
    }
    Ok(())
}
