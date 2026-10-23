//! Running other programs: ecash-wallet, the mints' `--version` and `pubkey`,
//! bitcoind's `--version`, and the service manager. A trait, so that what
//! `spi` does with a program's answer can be tested without the program;
//! [`System`] is the real one.

use std::process::{Command, Stdio};

/// What a program printed and how it ended. `code` is `None` when a signal
/// ended it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// The last line of stderr, else of stdout: what to say when it failed.
    pub fn last_line(&self) -> String {
        let pick = |text: &str| text.trim_end().lines().last().map(str::to_string);
        pick(&self.stderr)
            .or_else(|| pick(&self.stdout))
            .unwrap_or_else(|| format!("exit status {:?}", self.code))
    }
}

pub trait Exec {
    /// Run `bin` with `args` and the extra environment `env`. `Err` only when
    /// the program cannot be run at all.
    fn run(&self, bin: &str, args: &[String], env: &[(String, String)]) -> Result<Ran, String>;
}

/// The real thing: the program runs on this machine, with no stdin.
pub struct System;

impl Exec for System {
    fn run(&self, bin: &str, args: &[String], env: &[(String, String)]) -> Result<Ran, String> {
        let output = Command::new(bin)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run `{bin}`: {e}"))?;
        Ok(Ran {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

/// The words of a command line for a message.
pub fn command_line(bin: &str, args: &[String]) -> String {
    std::iter::once(bin.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ")
}
