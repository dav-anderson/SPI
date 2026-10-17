//! Start and stop the two mints as background processes of the current user.
//! (For a machine that boots and restarts them, `spi install` writes the launchd
//! or systemd definitions and a script that installs them; see `service`.)
//!
//! `start` writes the generated configs again first, so the files the mints
//! read are never older than `spi.toml`. A mint's pid is kept in
//! `<dir>/run/<mint>.pid`, its log in `<dir>/logs/<mint>.log`.
//!
//! **Order, and why psp-mint waits for the node.** `start` starts bitcoind
//! first (unless it is already answering, or `bitcoind.manage = false`), then
//! waits until bitcoind has finished its initial block download, and only then
//! starts psp-mint and esp-mint. The alternative, starting psp-mint at once
//! and letting it report not-ready, was rejected: psp-mint would be scanning a
//! chain that is behind, and would find, lock the price of and settle payments
//! on whatever part of the chain it can see, with the ESP mint and wallets
//! already pointed at it. Waiting means psp-mint never runs against a node
//! that is behind. The wait is bounded (`bitcoind.sync_wait_secs`, 600 by
//! default); when it runs out, `start` stops with the node's progress and
//! starts neither mint. bitcoind keeps syncing; run `start` again later.
//! `status` reports the same thing as "not ready" for a mint that was started
//! some other way.

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::bitcoind::{Chain, Node, Rpc};
use crate::config::{Network, Spi, Which};
use crate::{commands, generate, http};

fn read_pid(spi: &Spi, which: Which) -> Option<u32> {
    fs::read_to_string(spi.pid_file(which))
        .ok()
        .and_then(|t| t.trim().parse().ok())
}

/// Whether a process with this pid is running (`kill -0`).
pub fn alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn healthy(spi: &Spi, which: Which) -> bool {
    matches!(
        http::get_json(&format!("{}/health", spi.url(which))),
        Ok((200, _))
    )
}

fn wait_healthy(spi: &Spi, which: Which, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if healthy(spi, which) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Start one mint. Returns a line for the operator.
pub fn start_one(spi: &Spi, which: Which, wait_secs: u64) -> Result<String, String> {
    if let Some(pid) = read_pid(spi, which) {
        if alive(pid) {
            return Ok(format!("{} is already running (pid {pid})", which.name()));
        }
    }
    if healthy(spi, which) {
        return Err(format!(
            "something already answers at {}: stop it first (spi did not start it)",
            spi.url(which)
        ));
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(spi.log_file(which))
        .map_err(|e| format!("cannot open {}: {e}", spi.log_file(which).display()))?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&spi.mint(which).bin);
    cmd.arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    for (k, v) in generate::env(spi, which) {
        cmd.env(k, v);
    }
    // Its own process group, so a Ctrl-C at the terminal that ran `spi start`
    // does not stop it.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let child = cmd.spawn().map_err(|e| {
        format!(
            "cannot run `{}`: {e}. Put it on PATH or set {}.bin in spi.toml",
            spi.mint(which).bin,
            which.env_prefix().to_lowercase()
        )
    })?;
    let pid = child.id();
    fs::write(spi.pid_file(which), format!("{pid}\n")).map_err(|e| e.to_string())?;
    if wait_healthy(spi, which, wait_secs) {
        Ok(format!(
            "{} started (pid {pid}), healthy at {}",
            which.name(),
            spi.url(which)
        ))
    } else {
        Err(format!(
            "{} started (pid {pid}) but did not answer /health within {wait_secs} s: see {}",
            which.name(),
            spi.log_file(which).display()
        ))
    }
}

fn bitcoind_marker(spi: &Spi) -> std::path::PathBuf {
    spi.dir.join("run").join("bitcoind.started-by-spi")
}

/// The option that selects the chain on bitcoind's command line. Main has none.
pub fn chain_flag(network: Network) -> Option<&'static str> {
    match network {
        Network::Main => None,
        Network::Regtest => Some("-regtest"),
        Network::Signet => Some("-signet"),
        Network::Testnet => Some("-testnet"),
    }
}

/// The command line `spi` starts bitcoind with.
pub fn bitcoind_command_line(spi: &Spi) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(flag) = chain_flag(spi.network) {
        args.push(flag.to_string());
    }
    if let Some(d) = &spi.bitcoind_datadir {
        args.push(format!("-datadir={d}"));
    }
    args.push("-daemon".to_string());
    args.extend(spi.bitcoind_args.iter().cloned());
    args
}

/// Start bitcoind if it is not answering. Returns a line for the operator.
pub fn start_bitcoind(spi: &Spi, wait_secs: u64) -> Result<String, String> {
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    match rpc.node(spi.network.chain()) {
        Node::Syncing(_) | Node::Synced(_) => {
            return Ok(format!("bitcoind is already running at {}", spi.rpc_url));
        }
        Node::WrongChain { found } => {
            return Err(format!(
                "bitcoind at {} is on `{found}`, but the config says {}",
                spi.rpc_url,
                spi.network.name()
            ));
        }
        Node::Unreachable(_) => {}
    }
    if !spi.manage_bitcoind {
        return Err(format!(
            "bitcoind is unreachable at {} and bitcoind.manage is false: start it yourself",
            spi.rpc_url
        ));
    }
    let args = bitcoind_command_line(spi);
    let output = Command::new(&spi.bitcoind_bin)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            format!(
                "cannot run `{}`: {e}. Put it on PATH or set bitcoind.bin in spi.toml",
                spi.bitcoind_bin
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "`{} {}` failed: {}{}",
            spi.bitcoind_bin,
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let _ = fs::write(bitcoind_marker(spi), "started by spi\n");
    let end = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        match rpc.node(spi.network.chain()) {
            Node::Unreachable(e) => {
                if Instant::now() >= end {
                    return Err(format!(
                        "bitcoind was started but is not answering after {wait_secs} s: {e}"
                    ));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            _ => {
                return Ok(format!(
                    "bitcoind started (`{} {}`)",
                    spi.bitcoind_bin,
                    args.join(" ")
                ));
            }
        }
    }
}

/// Wait until bitcoind has finished its initial block download. `Ok` with the
/// node's state once it has; `Err` with the reason if the chain is wrong, or
/// the wait (`timeout_secs`) runs out while it is still syncing or
/// unreachable. `progress` is told each new state, once.
pub fn wait_for_sync(
    spi: &Spi,
    timeout_secs: u64,
    poll: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<Chain, String> {
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    let end = Instant::now() + Duration::from_secs(timeout_secs);
    let mut last = String::new();
    loop {
        let node = rpc.node(spi.network.chain());
        let line = match &node {
            Node::Synced(c) => return Ok(c.clone()),
            Node::WrongChain { found } => {
                return Err(format!(
                    "bitcoind is on `{found}`, but the config says {}",
                    spi.network.name()
                ));
            }
            Node::Syncing(c) => format!("bitcoind syncing {}%", c.percent()),
            Node::Unreachable(e) => format!("bitcoind unreachable: {e}"),
        };
        if line != last {
            progress(&line);
            last = line.clone();
        }
        if Instant::now() >= end {
            return Err(format!(
                "{line} after waiting {timeout_secs} s: psp-mint and esp-mint were NOT started. bitcoind keeps going; run `spi start` again when `spi status` shows it synced"
            ));
        }
        std::thread::sleep(poll);
    }
}

/// Start bitcoind (if needed), wait for it to finish syncing, then start
/// psp-mint and esp-mint (which needs the PSP).
pub fn start(spi: &Spi, wait_secs: u64) -> Result<Vec<String>, String> {
    commands::write_configs(spi)?;
    let mut lines = vec![format!("network: {}", spi.network.name())];
    lines.push(start_bitcoind(spi, 120)?);
    let mut seen = Vec::new();
    let waited = wait_for_sync(
        spi,
        spi.sync_wait_secs,
        Duration::from_secs(2),
        &mut |l: &str| seen.push(l.to_string()),
    );
    lines.extend(seen);
    match waited {
        Ok(c) => lines.push(format!("bitcoind {}", c.describe())),
        Err(e) => return Err(format!("{}\n{e}", lines.join("\n"))),
    }
    for which in [Which::Psp, Which::Esp] {
        lines.push(start_one(spi, which, wait_secs)?);
    }
    Ok(lines)
}

/// Stop one mint with SIGTERM and wait for it to finish its requests.
pub fn stop_one(spi: &Spi, which: Which, wait_secs: u64) -> Result<String, String> {
    let Some(pid) = read_pid(spi, which) else {
        return Ok(format!(
            "{} was not started by spi (no pid file)",
            which.name()
        ));
    };
    if !alive(pid) {
        let _ = fs::remove_file(spi.pid_file(which));
        return Ok(format!(
            "{} was not running (stale pid {pid})",
            which.name()
        ));
    }
    let sent = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map_err(|e| e.to_string())?;
    if !sent.success() {
        return Err(format!("cannot signal pid {pid}"));
    }
    let end = Instant::now() + Duration::from_secs(wait_secs);
    while Instant::now() < end {
        if !alive(pid) {
            let _ = fs::remove_file(spi.pid_file(which));
            return Ok(format!("{} stopped", which.name()));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "{} (pid {pid}) is still running {wait_secs} s after SIGTERM",
        which.name()
    ))
}

/// Stop bitcoind if `spi start` started it (never one it found running).
pub fn stop_bitcoind(spi: &Spi, wait_secs: u64) -> Result<Option<String>, String> {
    if !bitcoind_marker(spi).exists() {
        return Ok(None);
    }
    let rpc = Rpc::new(&spi.rpc_url, &spi.cookie);
    if matches!(rpc.node(spi.network.chain()), Node::Unreachable(_)) {
        let _ = fs::remove_file(bitcoind_marker(spi));
        return Ok(Some("bitcoind was not running".into()));
    }
    rpc.call(None, "stop", serde_json::json!([]))?;
    let end = Instant::now() + Duration::from_secs(wait_secs);
    while Instant::now() < end {
        if matches!(rpc.node(spi.network.chain()), Node::Unreachable(_)) {
            let _ = fs::remove_file(bitcoind_marker(spi));
            return Ok(Some("bitcoind stopped".into()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "bitcoind is still answering {wait_secs} s after `stop`"
    ))
}

/// Stop esp-mint, then psp-mint, then bitcoind if spi started it.
pub fn stop(spi: &Spi, wait_secs: u64) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut failed = None;
    for which in [Which::Esp, Which::Psp] {
        match stop_one(spi, which, wait_secs) {
            Ok(l) => lines.push(l),
            Err(e) => {
                lines.push(e.clone());
                failed = Some(e);
            }
        }
    }
    match stop_bitcoind(spi, wait_secs) {
        Ok(Some(l)) => lines.push(l),
        Ok(None) => {}
        Err(e) => {
            lines.push(e.clone());
            failed = Some(e);
        }
    }
    match failed {
        None => Ok(lines),
        Some(e) => Err(format!("{}\n{e}", lines.join("\n"))),
    }
}
