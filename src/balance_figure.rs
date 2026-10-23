//! Telling psp-mint what the SPI's wallet holds, for its burn threshold.
//!
//! With `[psp] burn = true`, psp-mint reissues the PSP it is paid to the SPI's
//! wallet only while the wallet holds less than `burn_threshold_psp`, and burns
//! the rest. It cannot see the wallet, so `spi` publishes a figure for it to
//! read: `{"psp": W, "reissued": R, "at": T}`, in the file `[psp]
//! burn_balance_file` (psp-mint's `burn.balance_file`).
//!
//! - `W` is the wallet's PSP balance (`ecash-wallet balance`).
//! - `R` is psp-mint's own `reissued` total (`GET /audit`), read **after** `W`.
//!   psp-mint works out what the wallet holds at a spend as `W` plus whatever it
//!   has reissued since `R`. Reading the wallet first means a note reissued in
//!   between is in `R` and not in `W`, so psp-mint counts it as not held and
//!   keeps more for the SPI than the threshold asks: the error is towards
//!   keeping the revenue, never towards burning it.
//! - `T` is the time (seconds) when the balance was read. psp-mint ignores a
//!   figure older than its `burn.balance_max_age_secs` (24 hours by default)
//!   and then reissues every paid unit, so a `spi` that stops running costs
//!   nothing but the burning.
//!
//! The figure is published only while `[psp] burn` is true, by `spi collect`
//! (after it has taken the reissued PSP in, so the wallet holds it), `spi audit`
//! and, by hand, `spi publish-balance`. There is no timer: each publish runs
//! `ecash-wallet balance`, which talks to the AIR server and the PSP, and the
//! figure only has to be close. spi has no command that spends from the SPI's
//! wallet (the printed `buy-psp` and the like are run with ecash-wallet
//! directly), so a spend is not seen until the next collect, audit or manual
//! publish; until then psp-mint believes the wallet holds more than it does and
//! burns more than it should. A figure older than `burn.balance_max_age_secs`
//! (24 hours by default) is not trusted: psp-mint reissues every paid unit and
//! burns nothing. The file is written to a temporary name beside it and renamed
//! over it, mode 0640, so psp-mint never reads half of one.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Spi, Which};
use crate::exec::Exec;
use crate::http;
use crate::wallet::{self, Balance};

/// The default file name, the one psp-mint looks for.
pub const FILE: &str = "wallet_balance.json";

/// What `spi` publishes and psp-mint reads (`psp_mint::balance::Figure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Figure {
    pub psp: u64,
    pub reissued: u64,
    pub at: u64,
}

/// The figure for a wallet balance and psp-mint's `/audit` body, read in that
/// order and at `at`.
pub fn figure(balance: &Balance, psp_audit: &Value, at: u64) -> Result<Figure, String> {
    let psp = u64::try_from(balance.psp).map_err(|_| {
        format!(
            "the wallet's PSP balance, {}, is more than a figure can carry (64 bits)",
            balance.psp
        )
    })?;
    let reissued = psp_audit
        .get("reissued")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            "psp-mint's /audit has no whole-number `reissued`: it is a psp-mint older than the burn threshold's wallet balance".to_string()
        })?;
    Ok(Figure { psp, reissued, at })
}

/// Write `figure` to `path` atomically: a temporary file in the same folder, then
/// a rename. Mode 0640. The folder must exist (psp-mint makes its network folder
/// when it first starts; the service layout makes `spi-share`).
pub fn write(path: &Path, figure: &Figure) -> Result<(), String> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no folder to write in", path.display()))?;
    if !dir.is_dir() {
        return Err(format!(
            "{} does not exist, so the balance figure cannot be written to {}. psp-mint makes its network folder when it first starts; a service's spi-share folder is made by install-system.sh",
            dir.display(),
            path.display()
        ));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| FILE.to_string());
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let text = serde_json::to_string(figure).map_err(|e| e.to_string())?;
    let result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        options.mode(0o640);
        let mut file = options.open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        // The mode given at creation is subject to the umask: say it again.
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o640))?;
        fs::rename(&temp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "cannot write the balance figure {}: {e}",
            path.display()
        ));
    }
    Ok(())
}

/// What a publish did, for the command's output.
pub fn describe(path: &Path, f: &Figure) -> String {
    format!(
        "published the SPI wallet's balance for psp-mint's burn threshold: {} PSP held, {} PSP reissued so far, in {}",
        f.psp,
        f.reissued,
        path.display()
    )
}

/// Publish from a wallet balance already read and psp-mint's `/audit` body read
/// after it (`spi audit`). `at` is when the balance was read.
pub fn publish_read(
    spi: &Spi,
    balance: &Balance,
    psp_audit: &Value,
    at: u64,
) -> Result<String, String> {
    let f = figure(balance, psp_audit, at)?;
    let path = spi.balance_figure_file();
    write(&path, &f)?;
    Ok(describe(&path, &f))
}

/// Read the wallet's balance, then psp-mint's `/audit`, and publish them
/// (`spi collect`). `now` is the clock, read after the balance.
pub fn publish(spi: &Spi, exec: &dyn Exec, now: &dyn Fn() -> u64) -> Result<String, String> {
    let balance = wallet::balance(spi, exec)?;
    let at = now();
    let url = format!("{}/audit", spi.url(Which::Psp));
    let (code, body) = http::get_json(&url)?;
    if code != 200 {
        return Err(format!("{url} answered HTTP {code}: {body}"));
    }
    publish_read(spi, &balance, &body, at)
}

/// The clock, in seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
