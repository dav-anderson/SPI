//! The SPI's own ecash-wallet: how `spi` calls it, reads its balance and takes
//! in the PSP that psp-mint reissues to it.
//!
//! The wallet is the stock `ecash-wallet`, run with `--wallet` (the file),
//! `--config` (the settings file spi writes, `generate::render_wallet`) and the
//! network flag. spi never opens the wallet file itself.
//!
//! **Collecting.** With burning off (the default) psp-mint does not burn the PSP
//! it is paid: it makes the notes itself and writes them to
//! `revenue/<request hash>.json` in its data folder. `collect` hands each file
//! to `ecash-wallet receive-psp`, which swaps the notes at the PSP mint for
//! notes only the wallet knows. The names of the files taken in are kept in
//! `<spi folder>/wallet/received.txt`, so a file is not offered twice; a file
//! the wallet says it has already taken in (spi was stopped between the two
//! steps) is recorded and not an error.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::commands;
use crate::config::{Network, Spi, Which};
use crate::exec::{Exec, command_line};

/// The wallet's network flag.
pub fn network_flag(network: Network) -> &'static str {
    match network {
        Network::Main => "--mainnet",
        Network::Regtest => "--regtest",
        Network::Signet => "--signet",
        Network::Testnet => "--testnet",
    }
}

/// What every call to the wallet starts with.
pub fn base_args(spi: &Spi) -> Vec<String> {
    vec![
        "--wallet".to_string(),
        spi.wallet_file().display().to_string(),
        "--config".to_string(),
        spi.wallet_config().display().to_string(),
        network_flag(spi.network).to_string(),
    ]
}

/// The wallet's command line up to its command, for the operator to copy.
pub fn prefix(spi: &Spi) -> String {
    command_line(&spi.wallet_settings.bin, &base_args(spi))
}

/// What the wallet holds, in PSP and ESP units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Balance {
    pub psp: u128,
    pub esp: u128,
}

/// The `psp` and `esp` lines of `ecash-wallet balance`. A line that starts with
/// space is a detail of the line above and is not read.
pub fn parse_balance(text: &str) -> Result<Balance, String> {
    let mut psp = None;
    let mut esp = None;
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(name), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let slot = match name {
            "psp" => &mut psp,
            "esp" => &mut esp,
            _ => continue,
        };
        let n: u128 = value.parse().map_err(|_| {
            format!("the `{name}` line of the wallet's balance is not a number: {line}")
        })?;
        *slot = Some(n);
    }
    match (psp, esp) {
        (Some(psp), Some(esp)) => Ok(Balance { psp, esp }),
        _ => Err("the wallet's balance has no `psp` and `esp` lines".into()),
    }
}

/// `ecash-wallet balance`. It runs the wallet's startup steps (it picks up what
/// the mints have for it), so it needs the AIR server and the PSP.
pub fn balance(spi: &Spi, exec: &dyn Exec) -> Result<Balance, String> {
    let mut args = base_args(spi);
    args.push("balance".into());
    let ran = exec.run(&spi.wallet_settings.bin, &args, &[])?;
    if !ran.ok() {
        return Err(format!(
            "`ecash-wallet balance` failed: {}",
            ran.last_line()
        ));
    }
    parse_balance(&ran.stdout)
}

/// The names of the reissued-PSP files already taken in.
pub fn received_file(spi: &Spi) -> PathBuf {
    spi.dir.join("wallet").join("received.txt")
}

/// What `collect` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Collected {
    pub lines: Vec<String>,
    /// How many files the wallet took in now.
    pub taken: usize,
    /// Whether any file could not be taken in.
    pub failed: bool,
}

/// Take in every reissued-PSP file not yet taken in. `Err` when the files
/// cannot be listed or the wallet cannot be run at all; a file the wallet
/// refuses is reported in `lines` and sets `failed`, and the others go on.
pub fn collect(spi: &Spi, exec: &dyn Exec) -> Result<Collected, String> {
    // The wallet needs the PSP's DID to swap at the mint.
    if commands::did_of(spi, exec, Which::Psp)?.is_none() {
        return Err(
            "the wallet names the PSP by its DID, which spi does not know: write it in spi.toml as [psp] did (what `psp-mint pubkey` prints)"
                .into(),
        );
    }
    commands::write_configs_with(spi, exec)?;
    let dir = spi.revenue_dir();
    let mut out = Collected::default();
    let mut names: Vec<String> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".json"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.lines.push(format!(
                "no PSP has been reissued to the SPI yet ({} does not exist)",
                dir.display()
            ));
            return Ok(out);
        }
        Err(e) => {
            return Err(format!(
                "cannot read {}: {e}. psp-mint writes it for its own account and group: run spi as a member of that group",
                dir.display()
            ));
        }
    };
    names.sort();
    let done = fs::read_to_string(received_file(spi)).unwrap_or_default();
    let done: Vec<&str> = done.lines().collect();
    out.lines
        .extend(prune_taken_in(spi, &dir, &mut names, &done));
    let pending: Vec<&String> = names
        .iter()
        .filter(|n| !done.contains(&n.as_str()))
        .collect();
    if pending.is_empty() {
        out.lines.push(format!(
            "nothing to collect: {} file(s) in {}, all taken in already",
            names.len(),
            dir.display()
        ));
        return Ok(out);
    }
    for name in pending {
        let mut args = base_args(spi);
        args.push("receive-psp".into());
        args.push(dir.join(name).display().to_string());
        let ran = exec.run(&spi.wallet_settings.bin, &args, &[])?;
        let already = ran.stderr.contains("imported already")
            || ran.stderr.contains("already in this wallet");
        if ran.ok() || already {
            record(spi, name)?;
            if ran.ok() {
                out.taken += 1;
                let first = ran.stdout.lines().next().unwrap_or("taken in");
                out.lines.push(format!("{name}: {first}"));
            } else {
                out.lines
                    .push(format!("{name}: the wallet had taken it in already"));
            }
        } else {
            out.failed = true;
            out.lines.push(format!("FAIL: {name}: {}", ran.last_line()));
        }
    }
    Ok(out)
}

/// Remove the files taken in more than `[psp] revenue_keep_days` ago (by the
/// file's age), and their lines in `received.txt`. A file not yet taken in is
/// never removed. 0 keeps every file. A file that cannot be removed is kept.
fn prune_taken_in(spi: &Spi, dir: &Path, names: &mut Vec<String>, done: &[&str]) -> Option<String> {
    let limit = Duration::from_secs(spi.revenue_keep_days * 86_400);
    let old = |n: &str| {
        let age = fs::metadata(dir.join(n)).and_then(|m| m.modified());
        age.is_ok_and(|t| t.elapsed().is_ok_and(|age| age > limit))
    };
    if spi.revenue_keep_days == 0 {
        return None;
    }
    let gone: Vec<String> = names
        .iter()
        .filter(|n| done.contains(&n.as_str()) && old(n) && fs::remove_file(dir.join(n)).is_ok())
        .cloned()
        .collect();
    if gone.is_empty() {
        return None;
    }
    names.retain(|n| !gone.contains(n));
    let kept: String = done
        .iter()
        .filter(|l| !gone.iter().any(|g| g.as_str() == **l))
        .map(|l| format!("{l}\n"))
        .collect();
    let _ = fs::write(received_file(spi), kept);
    Some(format!(
        "removed {} reissued-PSP file(s) taken in more than {} days ago",
        gone.len(),
        spi.revenue_keep_days
    ))
}

fn record(spi: &Spi, name: &str) -> Result<(), String> {
    let path = received_file(spi);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    writeln!(f, "{name}").map_err(|e| format!("cannot write {}: {e}", path.display()))
}
