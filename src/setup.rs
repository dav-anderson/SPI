//! The steps from a fresh machine to a working SPI, and when it is ready.
//!
//! a. bitcoind is running, synced, with its wallet loaded.
//! b. The AIR server is running.
//! c. psp-mint is running.
//! d. esp-mint is running.
//! e. The SPI's own wallet holds PSP. **Required**: without it the SPI cannot
//!    pay the first fee or buy anything from itself. Two ways to fund it, both
//!    printed by `spi setup`: buy PSP (`buy-psp --mint-key DID`, pay the payload
//!    once, then `claim-psp` after its confirmations),
//!    or take in a PSP handoff (`handoff import --did-key-file`). The genesis
//!    SPI buys from itself with every fee off.
//! f. The SPI's backups are funded: FSP bought from another service, if the SPI
//!    wants backups (recommended). Not tracked: spi cannot tell whether the SPI
//!    wants them.
//!
//! The SPI is **ready** only when every required step is done, which is a to
//! e. There is no "open channels" step: nothing here uses channels.

use serde::Serialize;

use crate::commands::Component;
use crate::wallet::Balance;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Step {
    pub id: String,
    pub name: String,
    pub required: bool,
    pub done: bool,
    pub detail: String,
}

fn part<'a>(parts: &'a [Component], name: &str) -> Option<&'a Component> {
    parts.iter().find(|c| c.name == name)
}

/// The steps. `wallet` is asked only when a to d are done: the wallet needs the
/// AIR server and the PSP to answer.
pub fn steps(parts: &[Component], wallet: &dyn Fn() -> Result<Balance, String>) -> Vec<Step> {
    let mut out = Vec::new();
    for (id, name, what) in [
        ("a", "bitcoind", "bitcoind running, synced, wallet loaded"),
        ("b", "air_server", "AIR server running"),
        ("c", "psp-mint", "psp-mint running"),
        ("d", "esp-mint", "esp-mint running"),
    ] {
        let found = part(parts, name);
        out.push(Step {
            id: id.into(),
            name: what.into(),
            required: true,
            done: found.is_some_and(|c| c.ok),
            detail: match found {
                Some(c) => c.detail.clone(),
                None => "not checked".into(),
            },
        });
    }
    let first_four = out.iter().all(|s| s.done);
    let (done, detail) = if !first_four {
        (false, "waiting for steps a to d".to_string())
    } else {
        match wallet() {
            Ok(b) if b.psp > 0 => (true, format!("the SPI wallet holds {} PSP", b.psp)),
            Ok(_) => (
                false,
                "the SPI wallet holds no PSP: fund it (`spi setup` shows both ways)".to_string(),
            ),
            Err(e) => (false, format!("cannot read the SPI wallet: {e}")),
        }
    };
    out.push(Step {
        id: "e".into(),
        name: "PSP in the SPI wallet".into(),
        required: true,
        done,
        detail,
    });
    out.push(Step {
        id: "f".into(),
        name: "FSP bought from another service, for backups".into(),
        required: false,
        done: false,
        detail: "recommended if the SPI wants backups; spi does not track it".into(),
    });
    out
}

/// Ready: every required step is done.
pub fn ready(steps: &[Step]) -> bool {
    steps.iter().filter(|s| s.required).all(|s| s.done)
}
