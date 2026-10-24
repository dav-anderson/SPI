//! `spi`: one config and one command line over psp-mint, esp-mint, bitcoind,
//! the AIR server and the SPI's own ecash-wallet. It does not link the mints:
//! it writes their config files, starts and stops their binaries, and reads
//! their HTTP endpoints.
//!
//! What it shares with `aso` (running programs, templating, releases, the
//! upgrade, the scheduled upgrade, the logs) is the `air-minterface` crate;
//! [`interface`] says what spi tells it about itself and its programs.

pub mod audit;
pub mod balance_figure;
pub mod bitcoind;
pub mod commands;
pub mod config;
pub mod fetch;
pub mod generate;
pub mod http;
pub mod interface;
pub mod lockfile;
pub mod run;
pub mod service;
pub mod setup;
pub mod signature;
pub mod upgrade;
pub mod version;
pub mod versions;
pub mod wallet;

pub use air_minterface::{exec, logs, release, schedule, template};
