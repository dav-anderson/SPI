//! `spi`: one config and one command line over psp-mint, esp-mint, bitcoind,
//! the AIR server and the SPI's own ecash-wallet. It does not link the mints:
//! it writes their config files, starts and stops their binaries, and reads
//! their HTTP endpoints.

pub mod audit;
pub mod balance_figure;
pub mod bitcoind;
pub mod commands;
pub mod config;
pub mod exec;
pub mod fetch;
pub mod generate;
pub mod http;
pub mod lockfile;
pub mod release;
pub mod run;
pub mod schedule;
pub mod service;
pub mod setup;
pub mod signature;
pub mod upgrade;
pub mod version;
pub mod versions;
pub mod wallet;
