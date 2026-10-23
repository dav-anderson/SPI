//! The time and the job behind `spi upgrade --at` / `--in`, `status` and
//! `cancel`: parsing, the next time the clock shows a time, the exact words,
//! the unit and plist text, the commands, the pending record, and what the
//! scheduled run decides from the clock.
//!
//! The clock is a fixed moment in `America/New_York`: Friday 2026-10-23 11:32
//! EDT. Nothing here reads the real clock or the real service manager; the job
//! files go to a folder of the test's own.

mod common;

use std::path::PathBuf;

use common::{Fake, fake, load_env, ran, scratch};
use jiff::Zoned;
use jiff::civil;
use spi::config::{Inputs, Spi};
use spi::schedule::{self, Due, Pending, Where};

/// Friday 2026-10-23 at `hour:minute` in New York.
fn friday(hour: i8, minute: i8) -> Zoned {
    civil::date(2026, 10, 23)
        .at(hour, minute, 0, 0)
        .in_tz("America/New_York")
        .unwrap()
}

fn now() -> Zoned {
    friday(11, 32)
}

struct Job {
    spi: Spi,
    w: Where,
}

fn job(name: &str, mac: bool) -> Job {
    let dir = scratch(name);
    let no_env = |_: &str| -> Option<String> { None };
    let spi = Spi::load(&Inputs {
        dir: dir.clone(),
        file: Some(""),
        env: &no_env,
        home: "/home/test".into(),
        os: if mac { "macos" } else { "linux" },
    })
    .unwrap();
    let w = Where {
        exe: PathBuf::from("/usr/local/bin/spi"),
        dir: dir.clone(),
        config: dir.join("spi.toml"),
        unit_dir: dir.join("units"),
    };
    Job { spi, w }
}

/// A service manager that accepts every command.
fn accepting() -> Fake {
    fake(|_, _| ran(0, "", ""))
}

fn calls(exec: &Fake) -> Vec<String> {
    exec.calls.lock().unwrap().clone()
}

fn pending(due: &Zoned) -> Pending {
    Pending {
        due: due.timestamp().as_second(),
        created: now().timestamp().as_second(),
        programs: vec!["psp-mint".into()],
        late_secs: schedule::DEFAULT_LATE_SECS,
        reset: vec![],
    }
}

// ---- parsing ----

#[test]
fn at_takes_a_24_hour_time() {
    assert_eq!(schedule::parse_at("03:00"), Ok((3, 0)));
    assert_eq!(schedule::parse_at("3:05"), Ok((3, 5)));
    assert_eq!(schedule::parse_at("23:59"), Ok((23, 59)));
    assert_eq!(schedule::parse_at("00:00"), Ok((0, 0)));
}

#[test]
fn at_refuses_what_is_not_a_time_and_says_what_it_takes() {
    for bad in [
        "24:00", "03:60", "3:5", "0300", "", "ab:cd", "-1:00", "03:00:00", "3pm", " 3:00",
    ] {
        assert_eq!(
            schedule::parse_at(bad),
            Err(format!(
                "`--at` takes a time like 03:00 (24-hour clock, this machine's local time), not `{bad}`"
            )),
            "{bad:?}"
        );
    }
}

#[test]
fn in_takes_days_hours_and_minutes() {
    assert_eq!(schedule::parse_duration("6h"), Ok(21_600));
    assert_eq!(schedule::parse_duration("90m"), Ok(5_400));
    assert_eq!(schedule::parse_duration("1h30m"), Ok(5_400));
    assert_eq!(schedule::parse_duration("1d"), Ok(86_400));
    assert_eq!(schedule::parse_duration("1d2h3m"), Ok(93_780));
    assert_eq!(schedule::parse_duration("1m"), Ok(60));
    assert_eq!(schedule::parse_duration("31d"), Ok(2_678_400));
}

#[test]
fn in_refuses_what_is_not_a_duration_or_is_out_of_range() {
    for bad in [
        "", "30s", "6", "h", "6 h", "0m", "32d", "-5m", "1h30", "6hours", "1.5h",
    ] {
        assert_eq!(
            schedule::parse_duration(bad),
            Err(format!(
                "`--in` takes a duration like 6h, 90m or 1h30m (days, hours, minutes), not `{bad}`"
            )),
            "{bad:?}"
        );
    }
}

// ---- the clock ----

#[test]
fn a_time_later_today_is_today_and_an_earlier_one_is_tomorrow() {
    // 11:32 now: 14:00 is today, 03:00 is Saturday.
    let today = schedule::next_at(&now(), 14, 0).unwrap();
    assert_eq!(
        today.strftime("%Y-%m-%d %H:%M").to_string(),
        "2026-10-23 14:00"
    );
    let tomorrow = schedule::next_at(&now(), 3, 0).unwrap();
    assert_eq!(
        tomorrow.strftime("%Y-%m-%d %H:%M").to_string(),
        "2026-10-24 03:00"
    );
    // The minute that is now is not later than now: it is tomorrow.
    let same = schedule::next_at(&now(), 11, 32).unwrap();
    assert_eq!(
        same.strftime("%Y-%m-%d %H:%M").to_string(),
        "2026-10-24 11:32"
    );
}

#[test]
fn the_scheduled_line_is_the_one_the_brief_names() {
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    assert_eq!(
        schedule::describe(&due, &now()),
        "Sat Oct 24 03:00 EDT (in 15h 28m)"
    );
}

#[test]
fn in_adds_real_time_to_now() {
    let due = schedule::after(&now(), 5_400).unwrap();
    assert_eq!(
        schedule::describe(&due, &now()),
        "Fri Oct 23 13:02 EDT (in 1h 30m)"
    );
    let due = schedule::after(&now(), 90_000).unwrap();
    assert_eq!(
        schedule::describe(&due, &now()),
        "Sat Oct 24 12:32 EDT (in 1d 1h)"
    );
}

#[test]
fn the_wait_across_the_autumn_clock_change_is_the_elapsed_time() {
    // 2026-11-01 00:30 EDT; the clocks go back at 02:00 EDT. 03:00 that morning
    // is EST, and is 3h30m away, not 2h30m.
    let start = civil::date(2026, 11, 1)
        .at(0, 30, 0, 0)
        .in_tz("America/New_York")
        .unwrap();
    let due = schedule::next_at(&start, 3, 0).unwrap();
    assert_eq!(
        schedule::describe(&due, &start),
        "Sun Nov 1 03:00 EST (in 3h 30m)"
    );
}

#[test]
fn a_time_that_has_passed_is_said_as_ago() {
    let due = friday(9, 2);
    assert_eq!(
        schedule::describe(&due, &now()),
        "Fri Oct 23 09:02 EDT (2h 30m ago)"
    );
}

#[test]
fn spans_are_days_hours_minutes() {
    assert_eq!(schedule::span_text(0), "0m");
    assert_eq!(schedule::span_text(59), "0m");
    assert_eq!(schedule::span_text(60), "1m");
    assert_eq!(schedule::span_text(3_600), "1h");
    assert_eq!(schedule::span_text(86_400 + 3_600 + 60), "1d 1h 1m");
    assert_eq!(schedule::span_text(86_400), "1d");
}

#[test]
fn an_epoch_is_read_back_in_the_zone_it_is_asked_for() {
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let back = schedule::zoned(due.timestamp().as_second(), now().time_zone()).unwrap();
    assert_eq!(
        back.strftime("%Y-%m-%d %H:%M %Z").to_string(),
        "2026-10-24 03:00 EDT"
    );
}

// ---- the job's files ----

#[test]
fn on_linux_the_job_is_a_unit_and_a_timer_set_in_utc() {
    let j = job("render-linux", false);
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let files = schedule::render(&j.spi, &j.w, &due).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, j.w.unit_dir.join("sp-upgrade.service"));
    assert_eq!(files[1].0, j.w.unit_dir.join("sp-upgrade.timer"));
    let service = &files[0].1;
    let timer = &files[1].1;
    let run = format!(
        "ExecStart=/usr/local/bin/spi --dir {} --config {} upgrade run-scheduled",
        j.w.dir.display(),
        j.w.config.display()
    );
    assert!(service.lines().any(|l| l == run), "{service}");
    assert!(service.contains("Type=oneshot"), "{service}");
    assert!(service.contains("Environment=HOME=/home/test"), "{service}");
    assert!(
        service.contains("StandardOutput=append:/var/log/sp-mint/spi-upgrade.log"),
        "{service}"
    );
    // 03:00 EDT is 07:00 UTC: the timer is written in UTC, so a change of the
    // local zone cannot move it.
    assert!(
        timer
            .lines()
            .any(|l| l == "OnCalendar=2026-10-24 07:00:00 UTC"),
        "{timer}"
    );
    // A machine that was off at that time runs it after it boots.
    assert!(timer.lines().any(|l| l == "Persistent=true"), "{timer}");
    assert!(timer.lines().any(|l| l == "OnBootSec=2min"), "{timer}");
    assert!(
        timer.lines().any(|l| l == "Unit=sp-upgrade.service"),
        "{timer}"
    );
    assert!(!service.contains("{{") && !timer.contains("{{"));
}

#[test]
fn on_macos_the_job_is_a_launchd_plist_with_the_local_time() {
    let j = job("render-mac", true);
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let files = schedule::render(&j.spi, &j.w, &due).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0].0,
        j.w.unit_dir.join("org.servicepoints.upgrade.plist")
    );
    let plist = &files[0].1;
    assert!(
        plist.contains("<key>Label</key>\n    <string>org.servicepoints.upgrade</string>"),
        "{plist}"
    );
    for (key, value) in [("Month", 10), ("Day", 24), ("Hour", 3), ("Minute", 0)] {
        assert!(
            plist.contains(&format!(
                "<key>{key}</key>\n        <integer>{value}</integer>"
            )),
            "{key} in {plist}"
        );
    }
    assert!(
        plist.contains("<key>RunAtLoad</key>\n    <true/>"),
        "{plist}"
    );
    assert!(plist.contains("<string>run-scheduled</string>"), "{plist}");
    assert!(!plist.contains("{{"));
}

#[test]
fn a_path_that_a_service_file_cannot_hold_is_refused_before_anything_is_written() {
    let mut j = job("render-bad-path", false);
    j.w.exe = PathBuf::from("/opt/my spi/spi");
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let why = schedule::render(&j.spi, &j.w, &due).unwrap_err();
    assert!(
        why.starts_with("the path of spi is `/opt/my spi/spi`"),
        "{why}"
    );
}

// ---- scheduling, status, cancel ----

#[test]
fn scheduling_writes_the_record_the_files_and_starts_the_timer() {
    let j = job("schedule-linux", false);
    let exec = accepting();
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let line = schedule::schedule(&j.spi, &j.w, &exec, &pending(&due), &due, &now()).unwrap();
    assert_eq!(
        line,
        "Upgrade scheduled for Sat Oct 24 03:00 EDT (in 15h 28m)"
    );
    assert!(j.w.unit_dir.join("sp-upgrade.service").is_file());
    assert!(j.w.unit_dir.join("sp-upgrade.timer").is_file());
    assert_eq!(
        calls(&exec),
        [
            "systemctl daemon-reload",
            "systemctl enable --now sp-upgrade.timer"
        ]
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(j.spi.dir.join("upgrade/pending.json")).unwrap())
            .unwrap();
    assert_eq!(
        record,
        serde_json::json!({
            "due": 1_792_825_200_i64,
            "created": 1_792_769_520_i64,
            "programs": ["psp-mint"],
            "late_secs": 3600,
            "reset": []
        })
    );
}

#[test]
fn on_macos_scheduling_loads_the_plist_and_cancel_unloads_it() {
    let j = job("schedule-mac", true);
    let exec = accepting();
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    schedule::schedule(&j.spi, &j.w, &exec, &pending(&due), &due, &now()).unwrap();
    let plist = j.w.unit_dir.join("org.servicepoints.upgrade.plist");
    assert!(plist.is_file());
    assert_eq!(
        calls(&exec),
        [format!("launchctl bootstrap system {}", plist.display())]
    );
    let exec = accepting();
    let line = schedule::cancel(&j.spi, &j.w, &exec, &now()).unwrap();
    assert_eq!(
        line,
        "Cancelled the upgrade scheduled for Sat Oct 24 03:00 EDT (in 15h 28m)."
    );
    assert_eq!(
        calls(&exec),
        ["launchctl bootout system/org.servicepoints.upgrade"]
    );
    assert!(!plist.exists());
    assert!(!j.spi.dir.join("upgrade/pending.json").exists());
}

#[test]
fn a_second_schedule_is_refused_and_the_first_is_left_as_it_was() {
    let j = job("schedule-twice", false);
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    schedule::schedule(&j.spi, &j.w, &accepting(), &pending(&due), &due, &now()).unwrap();
    let before = std::fs::read_to_string(j.spi.dir.join("upgrade/pending.json")).unwrap();
    let exec = accepting();
    let later = schedule::after(&now(), 21_600).unwrap();
    let why =
        schedule::schedule(&j.spi, &j.w, &exec, &pending(&later), &later, &now()).unwrap_err();
    assert_eq!(
        why,
        "an upgrade is already scheduled for Sat Oct 24 03:00 EDT (in 15h 28m): cancel it first with `spi upgrade cancel`"
    );
    assert!(calls(&exec).is_empty());
    assert_eq!(
        std::fs::read_to_string(j.spi.dir.join("upgrade/pending.json")).unwrap(),
        before
    );
    assert!(
        std::fs::read_to_string(j.w.unit_dir.join("sp-upgrade.timer"))
            .unwrap()
            .contains("OnCalendar=2026-10-24 07:00:00 UTC")
    );
}

#[test]
fn a_timer_the_service_manager_refuses_leaves_nothing_behind() {
    let j = job("schedule-refused", false);
    let exec = fake(|bin, args| {
        if bin == "systemctl" && args.first().map(String::as_str) == Some("enable") {
            ran(1, "", "Failed to enable unit: Access denied\n")
        } else {
            ran(0, "", "")
        }
    });
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let why = schedule::schedule(&j.spi, &j.w, &exec, &pending(&due), &due, &now()).unwrap_err();
    assert!(
        why.starts_with("`systemctl enable --now sp-upgrade.timer` failed:")
            && why.contains("Access denied"),
        "{why}"
    );
    assert!(!j.w.unit_dir.join("sp-upgrade.service").exists());
    assert!(!j.w.unit_dir.join("sp-upgrade.timer").exists());
    assert!(!j.spi.dir.join("upgrade/pending.json").exists());
}

#[test]
fn a_service_folder_spi_cannot_write_is_said_and_nothing_is_recorded() {
    let mut j = job("schedule-no-root", false);
    // A folder cannot be made under a file.
    let file = j.spi.dir.join("a-file");
    std::fs::write(&file, "x").unwrap();
    j.w.unit_dir = file.join("systemd");
    let exec = accepting();
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let why = schedule::schedule(&j.spi, &j.w, &exec, &pending(&due), &due, &now()).unwrap_err();
    assert!(
        why.starts_with("cannot create ")
            && why.ends_with("(run spi as root to schedule an upgrade)"),
        "{why}"
    );
    assert!(calls(&exec).is_empty());
    assert!(!j.spi.dir.join("upgrade/pending.json").exists());
}

#[test]
fn status_says_when_it_runs_and_that_it_asks_nothing() {
    let j = job("status-pending", false);
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    schedule::schedule(&j.spi, &j.w, &accepting(), &pending(&due), &due, &now()).unwrap();
    // An hour later.
    let later = friday(12, 32);
    assert_eq!(
        schedule::status(&j.spi, &j.w, &later).unwrap(),
        [
            "Upgrade scheduled for Sat Oct 24 03:00 EDT (in 14h 28m)",
            "programs: psp-mint",
            "It runs without asking. If the machine cannot start it within 1h of that time, it is skipped and removed."
        ]
    );
}

#[test]
fn status_names_all_four_when_none_was_chosen_and_says_none_when_none_is_scheduled() {
    let j = job("status-none", false);
    assert_eq!(
        schedule::status(&j.spi, &j.w, &now()).unwrap(),
        ["No upgrade is scheduled."]
    );
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    let mut p = pending(&due);
    p.programs = vec![];
    schedule::schedule(&j.spi, &j.w, &accepting(), &p, &due, &now()).unwrap();
    let lines = schedule::status(&j.spi, &j.w, &now()).unwrap();
    assert_eq!(
        lines[1],
        "programs: bitcoind, air_server, psp-mint and esp-mint (all four)"
    );
}

#[test]
fn status_warns_when_the_timer_file_is_gone_because_nothing_would_start_it() {
    let j = job("status-missing", false);
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    schedule::schedule(&j.spi, &j.w, &accepting(), &pending(&due), &due, &now()).unwrap();
    std::fs::remove_file(j.w.unit_dir.join("sp-upgrade.timer")).unwrap();
    let lines = schedule::status(&j.spi, &j.w, &now()).unwrap();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(
        lines[3],
        format!(
            "WARNING: {} is missing, so nothing will start it: `spi upgrade cancel`, then schedule it again",
            j.w.unit_dir.join("sp-upgrade.timer").display()
        )
    );
}

#[test]
fn cancel_takes_the_timer_out_and_deletes_the_files_and_the_record() {
    let j = job("cancel-linux", false);
    let due = schedule::next_at(&now(), 3, 0).unwrap();
    schedule::schedule(&j.spi, &j.w, &accepting(), &pending(&due), &due, &now()).unwrap();
    let exec = accepting();
    let line = schedule::cancel(&j.spi, &j.w, &exec, &now()).unwrap();
    assert_eq!(
        line,
        "Cancelled the upgrade scheduled for Sat Oct 24 03:00 EDT (in 15h 28m)."
    );
    assert_eq!(
        calls(&exec),
        [
            "systemctl disable --now sp-upgrade.timer",
            "systemctl daemon-reload"
        ]
    );
    assert!(!j.w.unit_dir.join("sp-upgrade.service").exists());
    assert!(!j.w.unit_dir.join("sp-upgrade.timer").exists());
    assert!(!j.spi.dir.join("upgrade/pending.json").exists());
    // And a new one can be scheduled now.
    schedule::schedule(&j.spi, &j.w, &accepting(), &pending(&due), &due, &now()).unwrap();
}

#[test]
fn cancel_with_nothing_scheduled_runs_nothing() {
    let j = job("cancel-none", false);
    let exec = accepting();
    assert_eq!(
        schedule::cancel(&j.spi, &j.w, &exec, &now()).unwrap(),
        "No upgrade is scheduled: nothing to cancel."
    );
    assert!(calls(&exec).is_empty());
}

// ---- what the scheduled run decides ----

fn write_record(j: &Job, due: i64, late: u64) {
    let path = j.spi.dir.join("upgrade/pending.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "{{\"due\": {due}, \"created\": 500, \"programs\": [\"esp-mint\"], \"late_secs\": {late}}}"
        ),
    )
    .unwrap();
}

#[test]
fn the_run_is_not_due_due_late_or_too_late_by_the_clock_alone() {
    let j = job("due-state", false);
    assert_eq!(schedule::due_state(&j.spi, 5_000), Ok(Due::Nothing));
    write_record(&j, 1_000, 3_600);
    let p = Pending {
        due: 1_000,
        created: 500,
        programs: vec!["esp-mint".into()],
        late_secs: 3_600,
        reset: vec![],
    };
    assert_eq!(schedule::due_state(&j.spi, 999), Ok(Due::NotYet(p.clone())));
    assert_eq!(schedule::due_state(&j.spi, 1_000), Ok(Due::Go(p.clone())));
    // One hour late is still allowed; one second more is not.
    assert_eq!(schedule::due_state(&j.spi, 4_600), Ok(Due::Go(p.clone())));
    assert_eq!(
        schedule::due_state(&j.spi, 4_601),
        Ok(Due::TooLate(p, 3_601))
    );
}

#[test]
fn a_record_that_is_not_one_is_said_and_not_guessed_at() {
    let j = job("due-garbage", false);
    let path = j.spi.dir.join("upgrade/pending.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{\"due\": \"soon\"}").unwrap();
    let why = schedule::due_state(&j.spi, 5_000).unwrap_err();
    assert!(
        why.starts_with(&format!("{} is not a scheduled upgrade:", path.display())),
        "{why}"
    );
}

#[test]
fn the_log_is_in_the_services_log_folder() {
    let dir = scratch("log-file");
    let spi = load_env(
        &dir,
        Some("[service]\nlog_dir = \"/var/log/elsewhere\"\n"),
        &[],
    )
    .unwrap();
    assert_eq!(
        schedule::log_file(&spi),
        PathBuf::from("/var/log/elsewhere/spi-upgrade.log")
    );
}
