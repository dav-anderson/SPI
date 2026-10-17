//! `spi`: one config and one command line over psp-mint, esp-mint and
//! bitcoind. It does not link the mints: it writes their config files, starts
//! and stops their binaries, and reads their HTTP endpoints.

pub mod audit;
pub mod bitcoind;
pub mod commands;
pub mod config;
pub mod generate;
pub mod http;
pub mod run;
pub mod service;
