//! `spi upgrade`'s programs: bitcoind, air_server, psp-mint and esp-mint, and
//! how each is restarted.
//!
//! The upgrade itself (the target, the build and its checksum, the swap, the
//! wait for health and the rollback) is air-minterface's `upgrade` module; this
//! one has what is spi's own: the programs, their restart commands and what
//! each restart costs (`Program::method`). The wording and the few rules that
//! differ per program are in [`crate::interface`]. Read air-minterface's
//! `upgrade` module for the steps.
//!
//! **Brief downtime.** Every upgrade is a restart of the program on the service
//! install: the old process stops, the new build starts. Nothing is upgraded
//! while it runs. (Upgrades with no downtime are not built; see ROADMAP item 25.)
//!
//! `commands::upgrade` warns and asks before anything is changed (a dry run
//! asks nothing and changes nothing). It needs the rights to write `bin_dir` and
//! to run the service manager, so run it as root (`sudo spi upgrade`).

use air_minterface::Managed;
use air_minterface::exec::Exec;
pub use air_minterface::upgrade::{Options, Upgraded, upgrade, wait_for};
pub use air_minterface::{Probe, Say};

use crate::config::Spi;

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

    pub fn is_mint(self) -> bool {
        matches!(self, Program::Psp | Program::Esp)
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

/// Restart one program through the service manager.
pub fn restart(spi: &Spi, exec: &dyn Exec, program: Program) -> Result<(), String> {
    let (bin, args) = restart_command(program, spi.mac);
    let ran = exec.run(&bin, &args, &[])?;
    if ran.ok() {
        Ok(())
    } else {
        Err(format!(
            "`{}` failed: {}",
            air_minterface::exec::command_line(&bin, &args),
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
