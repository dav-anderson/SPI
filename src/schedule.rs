//! A scheduled upgrade: `spi upgrade --at HH:MM` and `--in DURATION`, and
//! `spi upgrade status` and `spi upgrade cancel`.
//!
//! **One pending upgrade at a time.** Scheduling writes `<spi folder>/upgrade/
//! pending.json` (when it is due, which programs, how late it may start) and the
//! job that runs it; a second schedule is refused until `cancel`. Nothing is
//! scheduled unless `--at` or `--in` is given, and there is no spi.toml setting.
//!
//! **The job** runs `spi upgrade run-scheduled` as root, with no question asked
//! (the confirmation was given when it was scheduled):
//!
//! - Linux: a systemd timer and service, `sp-upgrade.timer` and
//!   `sp-upgrade.service`, in `/etc/systemd/system`, enabled so that they survive
//!   a reboot. The timer fires at the due time (written in UTC, so a change of
//!   the local clock or of the zone cannot move it) and, for a machine that was
//!   off then, `Persistent=true` and `OnBootSec` fire it after boot.
//! - macOS: a launchd job, `org.servicepoints.upgrade`, in `/Library/
//!   LaunchDaemons`, with `StartCalendarInterval` for the due time and `RunAtLoad`
//!   so that it also runs at boot.
//!
//! **What the run decides**, from the pending record and the clock (`due_state`):
//! not yet due: it does nothing and stays; due and no more than the allowed
//! lateness late: it upgrades; later than that: it **skips**, says so in the
//! log, and changes nothing. In the last two cases it then removes the job and
//! the record. The lateness is [`DEFAULT_LATE_SECS`] unless the record says
//! otherwise.
//!
//! Times are the machine's local clock; `jiff` reads the system zone.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::config::Spi;
use crate::exec::{Exec, command_line};
use crate::service::{expand, plain};

const SERVICE: &str = include_str!("../ops/systemd/upgrade.service");
const TIMER: &str = include_str!("../ops/systemd/upgrade.timer");
const PLIST: &str = include_str!("../ops/launchd/upgrade.plist");

/// How late a scheduled upgrade may start and still run: one hour. Later, it is
/// skipped. (Proposed in the report; it is the default of the record, not a
/// spi.toml setting.)
pub const DEFAULT_LATE_SECS: u64 = 3600;

/// The Linux unit name, and the launchd label, of the job.
pub const UNIT: &str = "sp-upgrade";
pub const LABEL: &str = "org.servicepoints.upgrade";

/// A scheduled upgrade, as kept in `pending.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// When it is due, in seconds since the Unix epoch.
    pub due: i64,
    /// When it was scheduled.
    pub created: i64,
    /// The programs to upgrade, by name; empty means all four.
    pub programs: Vec<String>,
    /// How many seconds late it may start and still run.
    pub late_secs: u64,
    /// The programs whose pin the operator chose to reset when scheduling; the
    /// run removes those pins from spi.toml before it upgrades.
    #[serde(default)]
    pub reset: Vec<String>,
}

/// Where the job lives and what it runs.
#[derive(Clone, Debug)]
pub struct Where {
    /// The `spi` binary the job runs.
    pub exe: PathBuf,
    /// The spi folder and the settings file the job reads.
    pub dir: PathBuf,
    pub config: PathBuf,
    /// Where the service manager's job files go: `/etc/systemd/system` on Linux,
    /// `/Library/LaunchDaemons` on macOS.
    pub unit_dir: PathBuf,
}

impl Where {
    /// The real places for this machine.
    pub fn system(spi: &Spi, exe: PathBuf, config: PathBuf) -> Where {
        Where {
            exe,
            dir: spi.dir.clone(),
            config,
            unit_dir: PathBuf::from(if spi.mac {
                "/Library/LaunchDaemons"
            } else {
                "/etc/systemd/system"
            }),
        }
    }

    /// The job's files, in the order they are written.
    pub fn files(&self, mac: bool) -> Vec<PathBuf> {
        if mac {
            vec![self.unit_dir.join(format!("{LABEL}.plist"))]
        } else {
            vec![
                self.unit_dir.join(format!("{UNIT}.service")),
                self.unit_dir.join(format!("{UNIT}.timer")),
            ]
        }
    }
}

// ---- time ----

/// `HH:MM` on the 24-hour clock: the hour and the minute.
pub fn parse_at(text: &str) -> Result<(i8, i8), String> {
    let bad = || {
        format!(
            "`--at` takes a time like 03:00 (24-hour clock, this machine's local time), not `{text}`"
        )
    };
    let (h, m) = text.split_once(':').ok_or_else(bad)?;
    let digits = |s: &str, len: std::ops::RangeInclusive<usize>| {
        len.contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit())
    };
    if !digits(h, 1..=2) || !digits(m, 2..=2) {
        return Err(bad());
    }
    let (h, m): (i8, i8) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
        return Err(bad());
    }
    Ok((h, m))
}

/// A duration like `6h`, `90m`, `1d` or `1h30m`, in seconds. At least one
/// minute and at most 31 days.
pub fn parse_duration(text: &str) -> Result<u64, String> {
    let bad = || {
        format!(
            "`--in` takes a duration like 6h, 90m or 1h30m (days, hours, minutes), not `{text}`"
        )
    };
    if text.is_empty() {
        return Err(bad());
    }
    let mut total: u64 = 0;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit = match c {
            'd' => 86_400,
            'h' => 3_600,
            'm' => 60,
            _ => return Err(bad()),
        };
        let n: u64 = number.parse().map_err(|_| bad())?;
        number.clear();
        total = total.saturating_add(n.saturating_mul(unit));
    }
    if !number.is_empty() || !(60..=31 * 86_400).contains(&total) {
        return Err(bad());
    }
    Ok(total)
}

/// The next time the clock shows `hour:minute`, after `now`, in `now`'s zone.
pub fn next_at(now: &Zoned, hour: i8, minute: i8) -> Result<Zoned, String> {
    let tz = now.time_zone().clone();
    let at = |date: jiff::civil::Date| -> Result<Zoned, String> {
        date.at(hour, minute, 0, 0)
            .to_zoned(tz.clone())
            .map_err(|e| format!("cannot work out {hour:02}:{minute:02}: {e}"))
    };
    let today = at(now.date())?;
    if today.timestamp() > now.timestamp() {
        return Ok(today);
    }
    let tomorrow = now
        .date()
        .tomorrow()
        .map_err(|e| format!("cannot work out tomorrow: {e}"))?;
    at(tomorrow)
}

/// `secs` after `now`, in `now`'s zone.
pub fn after(now: &Zoned, secs: u64) -> Result<Zoned, String> {
    let secs = i64::try_from(secs).map_err(|_| "that is too far away".to_string())?;
    let ts = Timestamp::from_second(now.timestamp().as_second().saturating_add(secs))
        .map_err(|e| format!("cannot add the time: {e}"))?;
    Ok(ts.to_zoned(now.time_zone().clone()))
}

/// The moment `epoch` in the zone `tz`.
pub fn zoned(epoch: i64, tz: &TimeZone) -> Result<Zoned, String> {
    Timestamp::from_second(epoch)
        .map(|t| t.to_zoned(tz.clone()))
        .map_err(|e| format!("{epoch} is not a time: {e}"))
}

/// A span of seconds as `1d 3h 5m`, `6h`, `28m`; under a minute is `0m`.
pub fn span_text(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3_600, (secs % 3_600) / 60);
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if parts.is_empty() {
        "0m".into()
    } else {
        parts.join(" ")
    }
}

/// `Sat Oct 24 03:00 EDT (in 15h 28m)`, or `(15m ago)` for a time that has passed.
pub fn describe(due: &Zoned, now: &Zoned) -> String {
    let when = due.strftime("%a %b %-d %H:%M %Z").to_string();
    let secs = due.timestamp().as_second() - now.timestamp().as_second();
    if secs >= 0 {
        format!("{when} (in {})", span_text(secs.unsigned_abs()))
    } else {
        format!("{when} ({} ago)", span_text(secs.unsigned_abs()))
    }
}

// ---- the record ----

pub fn pending_path(spi: &Spi) -> PathBuf {
    spi.dir.join("upgrade").join("pending.json")
}

/// The pending upgrade, if there is one.
pub fn read_pending(spi: &Spi) -> Result<Option<Pending>, String> {
    let path = pending_path(spi);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{} is not a scheduled upgrade: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_pending(spi: &Spi, pending: &Pending) -> Result<(), String> {
    let path = pending_path(spi);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(pending).map_err(|e| e.to_string())?;
    fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// What the pending upgrade is for, in words.
pub fn programs_text(programs: &[String]) -> String {
    if programs.is_empty() {
        "bitcoind, air_server, psp-mint and esp-mint (all four)".into()
    } else {
        programs.join(", ")
    }
}

// ---- the job ----

/// The text of each of the job's files, as `(path, text)`. Nothing is written.
pub fn render(spi: &Spi, w: &Where, due: &Zoned) -> Result<Vec<(PathBuf, String)>, String> {
    let exe = w.exe.display().to_string();
    let dir = w.dir.display().to_string();
    let config = w.config.display().to_string();
    for (what, value) in [
        ("the path of spi", exe.as_str()),
        ("the spi folder", dir.as_str()),
        ("the settings file", config.as_str()),
        ("the home folder", spi.home.as_str()),
        ("service.log_dir", spi.service.log_dir.as_str()),
    ] {
        plain(what, value)?;
    }
    let mut vars = vec![
        ("exe", exe),
        ("dir", dir),
        ("config", config),
        ("home", spi.home.clone()),
        ("log_dir", spi.service.log_dir.clone()),
    ];
    let files = w.files(spi.mac);
    if spi.mac {
        vars.push(("month", due.month().to_string()));
        vars.push(("day", due.day().to_string()));
        vars.push(("hour", due.hour().to_string()));
        vars.push(("minute", due.minute().to_string()));
        let text = expand("the upgrade property list", PLIST, &vars, true)?;
        Ok(vec![(files[0].clone(), text)])
    } else {
        let utc = due.with_time_zone(TimeZone::UTC);
        vars.push((
            "on_calendar",
            utc.strftime("%Y-%m-%d %H:%M:%S UTC").to_string(),
        ));
        let service = expand("the upgrade unit", SERVICE, &vars, false)?;
        let timer = expand("the upgrade timer", TIMER, &vars, false)?;
        Ok(vec![(files[0].clone(), service), (files[1].clone(), timer)])
    }
}

/// The commands that put the job in place, once its files are written.
pub fn start_commands(w: &Where, mac: bool) -> Vec<(String, Vec<String>)> {
    let cmd = |bin: &str, args: &[&str]| {
        (
            bin.to_string(),
            args.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        )
    };
    if mac {
        let plist = w
            .unit_dir
            .join(format!("{LABEL}.plist"))
            .display()
            .to_string();
        vec![(
            "launchctl".to_string(),
            vec!["bootstrap".to_string(), "system".to_string(), plist],
        )]
    } else {
        vec![
            cmd("systemctl", &["daemon-reload"]),
            cmd("systemctl", &["enable", "--now", &format!("{UNIT}.timer")]),
        ]
    }
}

/// The commands that take the job out of the service manager, before its files
/// are deleted.
pub fn stop_commands(mac: bool) -> Vec<(String, Vec<String>)> {
    if mac {
        vec![(
            "launchctl".to_string(),
            vec!["bootout".to_string(), format!("system/{LABEL}")],
        )]
    } else {
        vec![(
            "systemctl".to_string(),
            vec![
                "disable".to_string(),
                "--now".to_string(),
                format!("{UNIT}.timer"),
            ],
        )]
    }
}

fn run_all(exec: &dyn Exec, commands: &[(String, Vec<String>)]) -> Result<(), String> {
    for (bin, args) in commands {
        let ran = exec.run(bin, args, &[])?;
        if !ran.ok() {
            return Err(format!(
                "`{}` failed: {}",
                command_line(bin, args),
                ran.last_line()
            ));
        }
    }
    Ok(())
}

/// Take the job out of the service manager and delete its files and the record.
/// Everything is tried; the first thing that went wrong is returned.
pub fn remove(spi: &Spi, w: &Where, exec: &dyn Exec) -> Result<(), String> {
    let mut first: Option<String> = None;
    let mut note = |e: String| {
        first.get_or_insert(e);
    };
    // Not loaded is not an error here: the job may never have started.
    for (bin, args) in stop_commands(spi.mac) {
        if let Err(e) = exec.run(&bin, &args, &[]) {
            note(e);
        }
    }
    for file in w.files(spi.mac) {
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => note(format!("cannot delete {}: {e}", file.display())),
        }
    }
    if !spi.mac
        && let Err(e) = run_all(exec, &[("systemctl".into(), vec!["daemon-reload".into()])])
    {
        note(e);
    }
    match fs::remove_file(pending_path(spi)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => note(format!(
            "cannot delete {}: {e}",
            pending_path(spi).display()
        )),
    }
    match first {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// `Err` with the refusal when an upgrade is already pending: one at a time.
/// `spi upgrade --at` / `--in` asks this before it asks the operator anything.
pub fn refuse_if_pending(spi: &Spi, now: &Zoned) -> Result<(), String> {
    if let Some(p) = read_pending(spi)? {
        let when = zoned(p.due, now.time_zone())
            .map(|z| describe(&z, now))
            .unwrap_or_default();
        return Err(format!(
            "an upgrade is already scheduled for {when}: cancel it first with `spi upgrade cancel`"
        ));
    }
    Ok(())
}

/// Schedule the upgrade `pending` describes, due at `due`. Refuses when one is
/// already pending. Returns the line to print.
pub fn schedule(
    spi: &Spi,
    w: &Where,
    exec: &dyn Exec,
    pending: &Pending,
    due: &Zoned,
    now: &Zoned,
) -> Result<String, String> {
    refuse_if_pending(spi, now)?;
    let files = render(spi, w, due)?;
    let undo = |why: String| -> String {
        match remove(spi, w, exec) {
            Ok(()) => why,
            Err(e) => format!("{why} (and cleaning up failed: {e})"),
        }
    };
    if let Err(e) = fs::create_dir_all(&w.unit_dir) {
        return Err(format!(
            "cannot create {}: {e} (run spi as root to schedule an upgrade)",
            w.unit_dir.display()
        ));
    }
    write_pending(spi, pending)?;
    for (path, text) in &files {
        if let Err(e) = fs::write(path, text) {
            return Err(undo(format!(
                "cannot write {}: {e} (run spi as root to schedule an upgrade)",
                path.display()
            )));
        }
    }
    if let Err(e) = run_all(exec, &start_commands(w, spi.mac)) {
        return Err(undo(e));
    }
    Ok(format!("Upgrade scheduled for {}", describe(due, now)))
}

/// The lines of `spi upgrade status`.
pub fn status(spi: &Spi, w: &Where, now: &Zoned) -> Result<Vec<String>, String> {
    let Some(p) = read_pending(spi)? else {
        return Ok(vec!["No upgrade is scheduled.".into()]);
    };
    let due = zoned(p.due, now.time_zone())?;
    let mut lines = vec![
        format!("Upgrade scheduled for {}", describe(&due, now)),
        format!("programs: {}", programs_text(&p.programs)),
        format!(
            "It runs without asking. If the machine cannot start it within {} of that time, it is skipped and removed.",
            span_text(p.late_secs)
        ),
    ];
    for file in w.files(spi.mac) {
        if !file.is_file() {
            lines.push(format!(
                "WARNING: {} is missing, so nothing will start it: `spi upgrade cancel`, then schedule it again",
                file.display()
            ));
        }
    }
    Ok(lines)
}

/// Cancel the pending upgrade. Returns the line to print.
pub fn cancel(spi: &Spi, w: &Where, exec: &dyn Exec, now: &Zoned) -> Result<String, String> {
    let Some(p) = read_pending(spi)? else {
        return Ok("No upgrade is scheduled: nothing to cancel.".into());
    };
    let due = zoned(p.due, now.time_zone())?;
    let when = describe(&due, now);
    remove(spi, w, exec)?;
    Ok(format!("Cancelled the upgrade scheduled for {when}."))
}

/// What the clock says about a pending upgrade.
#[derive(Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing is scheduled.
    Nothing,
    /// It is not time yet; the job stays.
    NotYet(Pending),
    /// It is later than the lateness allows, by this many seconds.
    TooLate(Pending, u64),
    /// Go.
    Go(Pending),
}

/// Whether the pending upgrade is due at `now_secs`.
pub fn due_state(spi: &Spi, now_secs: i64) -> Result<Due, String> {
    let Some(p) = read_pending(spi)? else {
        return Ok(Due::Nothing);
    };
    if now_secs < p.due {
        return Ok(Due::NotYet(p));
    }
    let late = u64::try_from(now_secs - p.due).unwrap_or(0);
    if late > p.late_secs {
        Ok(Due::TooLate(p, late))
    } else {
        Ok(Due::Go(p))
    }
}

/// The folder the job writes its log to.
pub fn log_file(spi: &Spi) -> PathBuf {
    Path::new(&spi.service.log_dir).join("spi-upgrade.log")
}
