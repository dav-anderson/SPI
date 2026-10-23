//! What the operator sees and what changes when `spi upgrade`, `spi upgrade
//! check`, `status`, `cancel`, `spi restart` and `spi snapshot` run: the
//! warning and the confirmation first, a dry run that asks nothing, scheduling
//! (and the scheduled run that asks nothing), pins that are never silent, and
//! the service-manager commands.
//!
//! The programs are stood in for: a fake runner answers `--version` from the
//! content of the file it is asked to run, and records the service manager's
//! commands. The release source is a fake that serves from memory. The clock is
//! Friday 2026-10-23 11:32 in New York. Nothing is slept for.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use common::{Fake, FakeSource, fake, ran, scratch};
use jiff::{Zoned, civil};
use spi::commands::{self, Around, Outcome, PinPolicy, Prompts, UpgradeArgs, When};
use spi::config::{Inputs, Spi};
use spi::exec::System;
use spi::fetch::CurlSource;
use spi::release::{NoSource, Source};
use spi::schedule::{self, Where};
use spi::upgrade::{MINT_DOWNTIME, Probe, Program};

const WARNING: &str = "WARNING: spi upgrade shuts down the AIR service and every mint it runs (psp-mint, esp-mint), one program at a time, each for a brief downtime. Nothing is changed unless you confirm.";

const PIN_WARNING: [&str; 3] = [
    "WARNING: these programs are pinned and are not at the newest release:",
    "  psp-mint: pinned at 0.6.0 (newest 0.7.0)",
    "`spi upgrade` installs the pin unless it is reset. It asks about each pin (--keep-pins keeps them all, --reset-pins resets them all); a reset removes the `version` line from spi.toml (the file is backed up first).",
];

const PIN_QUESTION: &str = "psp-mint is pinned at 0.6.0; the newest release is 0.7.0. Keep the pin (0.6.0) or reset it and install 0.7.0? [keep/reset]";

fn clock(hour: i8, minute: i8) -> Zoned {
    civil::date(2026, 10, 23)
        .at(hour, minute, 0, 0)
        .in_tz("America/New_York")
        .unwrap()
}

fn now() -> Zoned {
    clock(11, 32)
}

// ---- the world ----

struct World {
    spi: Spi,
    dir: PathBuf,
    installed: PathBuf,
    config: PathBuf,
    job: Where,
    exec: Fake,
}

/// spi.toml as the operator wrote it: a comment on top and one above the psp
/// pin. `psp_pin` is the `version` under `[psp]`.
fn config_text(installed: &Path, new: &Path, psp_pin: Option<&str>) -> String {
    let pin = psp_pin
        .map(|v| format!("version = \"{v}\"\n"))
        .unwrap_or_default();
    format!(
        "# Our settings.\n[service]\nbin_dir = \"{i}\"\n\n[bitcoind]\nbin = \"{n}/bitcoind\"\n\n[air]\nbin = \"{n}/air_server\"\nversion = \"1.0.2\"\n\n[psp]\nbin = \"{n}/psp-mint\"\n# held back until the audit\n{pin}\n[esp]\nbin = \"{n}/esp-mint\"\n",
        i = installed.display(),
        n = new.display()
    )
}

/// A service install in a folder of its own: psp-mint 0.5.0, esp-mint 0.7.0,
/// bitcoind 29.0.0 and an air_server installed; a build of psp-mint 0.6.0 in
/// `new`. The file's content is what its `--version` prints.
fn world(name: &str, mac: bool, psp_pin: Option<&str>) -> World {
    let dir = scratch(name);
    let installed = dir.join("bin");
    let new = dir.join("new");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    for (n, old) in [
        ("bitcoind", "Bitcoin Core version v29.0.0\n"),
        ("psp-mint", "psp-mint 0.5.0\n"),
        ("esp-mint", "esp-mint 0.7.0\n"),
        ("air_server", "old air"),
    ] {
        std::fs::write(installed.join(n), old).unwrap();
    }
    std::fs::write(new.join("psp-mint"), "psp-mint 0.6.0\n").unwrap();
    let text = config_text(&installed, &new, psp_pin);
    let config = dir.join("spi.toml");
    std::fs::write(&config, &text).unwrap();
    let no_env = |_: &str| -> Option<String> { None };
    let spi = Spi::load(&Inputs {
        dir: dir.clone(),
        file: Some(&text),
        env: &no_env,
        home: "/home/test".into(),
        os: if mac { "macos" } else { "linux" },
    })
    .unwrap();
    let job = Where {
        exe: PathBuf::from("/usr/local/bin/spi"),
        dir: dir.clone(),
        config: config.clone(),
        unit_dir: dir.join("units"),
    };
    let exec = fake(|bin, args| {
        if bin == "systemctl" || bin == "launchctl" {
            ran(0, "", "")
        } else if args.len() == 1 && args[0] == "--version" {
            Ok(spi::exec::Ran {
                code: Some(0),
                stdout: std::fs::read_to_string(bin).map_err(|e| format!("{bin}: {e}"))?,
                stderr: String::new(),
            })
        } else {
            Err(format!("unexpected: {bin} {args:?}"))
        }
    });
    World {
        spi,
        dir,
        installed,
        config,
        job,
        exec,
    }
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The service manager's commands so far.
fn service_calls(w: &World) -> Vec<String> {
    w.exec
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.starts_with("systemctl") || c.starts_with("launchctl"))
        .cloned()
        .collect()
}

/// psp-mint 0.6.0 and 0.7.0 are released; 0.7.0 is the newest.
fn source() -> FakeSource {
    FakeSource::new()
        .tags(Program::Psp, &["v0.5.0", "v0.6.0", "v0.7.0", "v0.8.0-rc1"])
        .build(Program::Psp, "0.6.0", "psp-mint 0.6.0\n")
        .build(Program::Psp, "0.7.0", "psp-mint 0.7.0\n")
}

fn psp() -> Vec<String> {
    vec!["psp-mint".to_string()]
}

/// What was asked of the person and what came out.
struct Run {
    out: Outcome,
    /// Each confirmation, with the lines it showed.
    confirms: Vec<Vec<String>>,
    /// Each pin question, with the lines it showed.
    pins: Vec<Vec<String>>,
}

/// Run `f` with the prompts answered `(confirm, pin)` (`pin`: `Some(true)` is
/// reset, `Some(false)` keep, `None` no terminal), a mint that reports
/// `target` on /health, and the clock at `at`.
fn go(
    w: &World,
    source: &dyn Source,
    answers: (bool, Option<bool>),
    target: &'static str,
    at: &Zoned,
    f: &dyn Fn(&Spi, &Around) -> Outcome,
) -> Run {
    let confirms = Mutex::new(Vec::new());
    let pins = Mutex::new(Vec::new());
    let confirm = |lines: &[String]| {
        confirms.lock().unwrap().push(lines.to_vec());
        answers.0
    };
    let pin = |lines: &[String]| {
        pins.lock().unwrap().push(lines.to_vec());
        answers.1
    };
    let prompts = Prompts {
        confirm: &confirm,
        pin: &pin,
    };
    let probe = move |p: Program| {
        if p.is_mint() {
            Probe::mint(target)
        } else {
            Probe::up()
        }
    };
    let around = Around {
        exec: &w.exec,
        source,
        prompts: &prompts,
        probe: &probe,
        config: &w.config,
        now: at,
        job: &w.job,
    };
    let out = f(&w.spi, &around);
    Run {
        out,
        confirms: confirms.into_inner().unwrap(),
        pins: pins.into_inner().unwrap(),
    }
}

#[derive(Clone, Copy)]
struct Case {
    when: When,
    pins: PinPolicy,
    dry_run: bool,
}

const NOW: Case = Case {
    when: When::Now,
    pins: PinPolicy::Ask,
    dry_run: false,
};

fn upgrade_run(
    w: &World,
    source: &dyn Source,
    answers: (bool, Option<bool>),
    target: &'static str,
    case: Case,
) -> Run {
    let names = psp();
    go(w, source, answers, target, &now(), &|spi, around| {
        commands::upgrade(
            spi,
            &UpgradeArgs {
                names: &names,
                dry_run: case.dry_run,
                when: case.when,
                pins: case.pins,
                wait_secs: 0,
            },
            around,
        )
    })
}

fn scheduled_run(w: &World, source: &dyn Source, target: &'static str, at: &Zoned) -> Run {
    go(w, source, (false, None), target, at, &|spi, around| {
        commands::run_scheduled(spi, 0, around)
    })
}

fn last(run: &Run) -> &str {
    run.out.lines.last().map(String::as_str).unwrap_or("")
}

fn pending_json(w: &World) -> PathBuf {
    w.spi.dir.join("upgrade/pending.json")
}

// ---- the warning and the confirmation ----

#[test]
fn without_a_yes_nothing_changes_and_no_service_command_runs() {
    let w = world("no-yes", false, Some("0.6.0"));
    let before = text(&w.config);
    let run = upgrade_run(&w, &source(), (false, Some(false)), "0.6.0", NOW);
    assert!(!run.out.ok);
    assert_eq!(last(&run), "not confirmed: nothing was changed");
    // The question was asked once, and it opened with the warning.
    assert_eq!(run.confirms.len(), 1);
    assert_eq!(run.confirms[0][0], WARNING);
    assert_eq!(run.confirms[0][1], "This run restarts: psp-mint.");
    // bin_dir is as it was: the same files with the same content, and nothing new.
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert_eq!(text(&w.installed.join("esp-mint")), "esp-mint 0.7.0\n");
    let mut names: Vec<String> = std::fs::read_dir(&w.installed)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["air_server", "bitcoind", "esp-mint", "psp-mint"]);
    assert!(service_calls(&w).is_empty(), "{:?}", service_calls(&w));
    assert!(!w.spi.dir.join("releases").exists());
    assert_eq!(text(&w.config), before);
}

#[test]
fn with_a_yes_the_upgrade_runs_and_the_question_was_the_warning_and_the_plan() {
    let w = world("yes", false, Some("0.6.0"));
    let run = upgrade_run(&w, &source(), (true, Some(false)), "0.6.0", NOW);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(last(&run), "psp-mint: now 0.6.0, healthy");
    assert_eq!(run.confirms.len(), 1);
    assert_eq!(run.confirms[0][0], WARNING);
    assert!(
        run.confirms[0].contains(&format!("psp-mint: 0.5.0 -> 0.6.0 by {MINT_DOWNTIME}")),
        "{:?}",
        run.confirms[0]
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.6.0\n");
    assert!(
        service_calls(&w).contains(&"systemctl restart psp-mint.service".to_string()),
        "{:?}",
        service_calls(&w)
    );
}

#[test]
fn a_dry_run_shows_the_warning_asks_nothing_and_changes_nothing() {
    let w = world("dry", false, Some("0.6.0"));
    let before = text(&w.config);
    let case = Case {
        dry_run: true,
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (false, Some(true)), "0.6.0", case);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert!(run.confirms.is_empty() && run.pins.is_empty());
    assert_eq!(run.out.lines[0], WARNING);
    assert_eq!(run.out.lines[1..4], PIN_WARNING);
    assert!(
        run.out
            .lines
            .contains(&format!("psp-mint: 0.5.0 -> 0.6.0 by {MINT_DOWNTIME}")),
        "{:?}",
        run.out.lines
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert!(service_calls(&w).is_empty());
    assert!(!w.spi.dir.join("releases").exists());
    assert_eq!(text(&w.config), before);
}

#[test]
fn a_dry_run_cannot_be_scheduled() {
    let w = world("dry-at", false, None);
    let case = Case {
        when: When::At(3, 0),
        dry_run: true,
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (true, None), "0.7.0", case);
    assert!(!run.out.ok);
    assert_eq!(
        run.out.lines,
        ["FAIL: --dry-run changes nothing, so it cannot be scheduled: leave out --at and --in"]
    );
    assert!(run.confirms.is_empty());
    assert!(!pending_json(&w).exists());
}

#[test]
fn nothing_to_change_asks_nothing_and_stops_nothing() {
    let w = world("up-to-date", false, None);
    std::fs::write(w.installed.join("psp-mint"), "psp-mint 0.7.0\n").unwrap();
    let run = upgrade_run(&w, &source(), (false, None), "0.7.0", NOW);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert!(run.confirms.is_empty());
    assert_eq!(
        run.out.lines,
        [
            "psp-mint: already 0.7.0: nothing to do",
            "everything up to date: nothing was stopped"
        ]
    );
    assert!(service_calls(&w).is_empty());
}

#[test]
fn a_program_spi_does_not_upgrade_is_named() {
    let w = world("not-a-program", false, None);
    let names = vec!["fsp-mint".to_string()];
    let run = go(
        &w,
        &NoSource,
        (true, None),
        "0.7.0",
        &now(),
        &|spi, around| {
            commands::upgrade(
                spi,
                &UpgradeArgs {
                    names: &names,
                    dry_run: false,
                    when: When::Now,
                    pins: PinPolicy::Ask,
                    wait_secs: 0,
                },
                around,
            )
        },
    );
    assert!(!run.out.ok);
    assert_eq!(
        run.out.lines,
        [
            "FAIL: `fsp-mint` is not something spi upgrades (bitcoind, air_server, psp-mint, esp-mint)"
        ]
    );
    assert!(run.confirms.is_empty());
}

// ---- scheduling ----

#[test]
fn scheduling_asks_first_and_writes_nothing_without_a_yes() {
    let w = world("schedule-no", false, None);
    let case = Case {
        when: When::At(3, 0),
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (false, None), "0.7.0", case);
    assert!(!run.out.ok);
    assert_eq!(
        last(&run),
        "not confirmed: nothing was scheduled and nothing was changed"
    );
    assert_eq!(
        run.confirms,
        [[
            WARNING.to_string(),
            "It will upgrade: psp-mint, without asking again.".to_string(),
            "It is scheduled for Sat Oct 24 03:00 EDT (in 15h 28m).".to_string()
        ]]
    );
    assert!(!pending_json(&w).exists());
    assert!(!w.job.unit_dir.exists());
    assert!(service_calls(&w).is_empty());
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
}

#[test]
fn scheduling_with_a_yes_records_it_prints_the_time_and_upgrades_nothing_yet() {
    let w = world("schedule-yes", false, None);
    let case = Case {
        when: When::At(3, 0),
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (true, None), "0.7.0", case);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(
        last(&run),
        "Upgrade scheduled for Sat Oct 24 03:00 EDT (in 15h 28m)"
    );
    assert_eq!(run.confirms.len(), 1);
    let record: serde_json::Value = serde_json::from_str(&text(&pending_json(&w))).unwrap();
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
    // Only the timer was set up; nothing was restarted and nothing installed.
    assert_eq!(
        service_calls(&w),
        [
            "systemctl daemon-reload",
            "systemctl enable --now sp-upgrade.timer"
        ]
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
}

#[test]
fn in_schedules_that_long_from_now() {
    let w = world("schedule-in", false, None);
    let case = Case {
        when: When::In(3 * 3600),
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (true, None), "0.7.0", case);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(
        last(&run),
        "Upgrade scheduled for Fri Oct 23 14:32 EDT (in 3h)"
    );
}

#[test]
fn a_second_schedule_is_refused_before_anything_is_asked() {
    let w = world("schedule-twice", false, None);
    let at = Case {
        when: When::At(3, 0),
        ..NOW
    };
    upgrade_run(&w, &source(), (true, None), "0.7.0", at);
    let before = text(&pending_json(&w));
    let again = Case {
        when: When::In(3600),
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (true, None), "0.7.0", again);
    assert!(!run.out.ok);
    assert_eq!(
        run.out.lines,
        [
            "FAIL: an upgrade is already scheduled for Sat Oct 24 03:00 EDT (in 15h 28m): cancel it first with `spi upgrade cancel`"
        ]
    );
    assert!(run.confirms.is_empty());
    assert_eq!(text(&pending_json(&w)), before);
}

#[test]
fn status_and_cancel_follow_the_schedule() {
    let w = world("status-cancel", false, None);
    let status = |w: &World| {
        go(w, &source(), (false, None), "0.7.0", &now(), &|s, a| {
            commands::upgrade_status(s, a)
        })
    };
    let cancel = |w: &World| {
        go(w, &source(), (false, None), "0.7.0", &now(), &|s, a| {
            commands::upgrade_cancel(s, a)
        })
    };
    assert_eq!(status(&w).out.lines, ["No upgrade is scheduled."]);
    assert_eq!(
        cancel(&w).out.lines,
        ["No upgrade is scheduled: nothing to cancel."]
    );
    let at = Case {
        when: When::At(3, 0),
        ..NOW
    };
    upgrade_run(&w, &source(), (true, None), "0.7.0", at);
    assert_eq!(
        status(&w).out.lines,
        [
            "Upgrade scheduled for Sat Oct 24 03:00 EDT (in 15h 28m)",
            "programs: psp-mint",
            "It runs without asking. If the machine cannot start it within 1h of that time, it is skipped and removed."
        ]
    );
    let done = cancel(&w);
    assert!(done.out.ok);
    assert_eq!(
        done.out.lines,
        ["Cancelled the upgrade scheduled for Sat Oct 24 03:00 EDT (in 15h 28m)."]
    );
    assert!(!pending_json(&w).exists());
    assert!(!w.job.unit_dir.join("sp-upgrade.timer").exists());
    assert_eq!(status(&w).out.lines, ["No upgrade is scheduled."]);
}

// ---- the scheduled run ----

fn scheduled(name: &str) -> World {
    let w = world(name, false, None);
    let run = upgrade_run(&w, &source(), (true, None), "0.7.0", AT_THREE);
    assert!(run.out.ok, "{:?}", run.out.lines);
    w
}

const AT_THREE: Case = Case {
    when: When::At(3, 0),
    pins: PinPolicy::Ask,
    dry_run: false,
};

/// Saturday 2026-10-24 03:00 EDT (07:00 UTC) is the epoch second 1792825200;
/// this is that moment plus `secs` (minus when negative).
fn due_plus(secs: i64) -> Zoned {
    schedule::zoned(1_792_825_200 + secs, now().time_zone()).unwrap()
}

#[test]
fn before_the_time_the_run_does_nothing_and_the_job_stays() {
    let w = scheduled("run-early");
    let run = scheduled_run(&w, &source(), "0.7.0", &due_plus(-60));
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(
        run.out.lines,
        ["not due yet (due Sat Oct 24 03:00 EDT (in 1m)): nothing to do"]
    );
    assert!(pending_json(&w).exists());
    assert!(w.job.unit_dir.join("sp-upgrade.timer").exists());
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
}

#[test]
fn at_the_time_the_run_upgrades_without_asking_and_removes_itself() {
    let w = scheduled("run-go");
    let run = scheduled_run(&w, &source(), "0.7.0", &due_plus(600));
    assert!(run.out.ok, "{:?}", run.out.lines);
    // It asked nothing.
    assert!(run.confirms.is_empty() && run.pins.is_empty());
    assert_eq!(
        run.out.lines[0],
        "running the upgrade scheduled for Sat Oct 24 03:00 EDT (10m ago)"
    );
    assert!(
        run.out
            .lines
            .contains(&"psp-mint: now 0.7.0, healthy".to_string()),
        "{:?}",
        run.out.lines
    );
    assert_eq!(last(&run), "the scheduled job removed itself");
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.7.0\n");
    assert!(!pending_json(&w).exists());
    assert!(!w.job.unit_dir.join("sp-upgrade.timer").exists());
    assert!(!w.job.unit_dir.join("sp-upgrade.service").exists());
}

#[test]
fn an_hour_late_still_runs_and_a_second_more_is_skipped() {
    let w = scheduled("run-edge-ok");
    let run = scheduled_run(&w, &source(), "0.7.0", &due_plus(3600));
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.7.0\n");

    let w = scheduled("run-edge-late");
    let run = scheduled_run(&w, &source(), "0.7.0", &due_plus(3601));
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(
        run.out.lines,
        [
            "skipped: the upgrade was due Sat Oct 24 03:00 EDT (1h ago) and it is now 1h later, more than the 1h allowed; nothing was changed",
            "the scheduled job removed itself"
        ]
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
}

#[test]
fn a_run_that_is_too_late_changes_nothing_and_removes_the_job() {
    let w = scheduled("run-late");
    let run = scheduled_run(&w, &source(), "0.7.0", &due_plus(2 * 3600));
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(
        run.out.lines,
        [
            "skipped: the upgrade was due Sat Oct 24 03:00 EDT (2h ago) and it is now 2h later, more than the 1h allowed; nothing was changed",
            "the scheduled job removed itself"
        ]
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert!(!pending_json(&w).exists());
    assert!(!w.job.unit_dir.join("sp-upgrade.timer").exists());
    assert!(
        !service_calls(&w).iter().any(|c| c.contains("restart")),
        "{:?}",
        service_calls(&w)
    );
}

#[test]
fn a_run_with_nothing_scheduled_says_so_and_cleans_up_after_itself() {
    let w = world("run-nothing", false, None);
    let run = scheduled_run(&w, &source(), "0.7.0", &now());
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(
        run.out.lines,
        [
            "no upgrade is scheduled: nothing to do",
            "the scheduled job removed itself"
        ]
    );
}

// ---- pins are never silent ----

#[test]
fn a_pin_behind_the_newest_is_warned_about_in_status_with_the_exact_lines() {
    let w = world("pin-status", false, Some("0.6.0"));
    let run = go(&w, &source(), (false, None), "0.6.0", &now(), &|s, a| {
        commands::upgrade_status(s, a)
    });
    let mut expected = vec!["No upgrade is scheduled.".to_string()];
    expected.extend(PIN_WARNING.map(String::from));
    assert_eq!(run.out.lines, expected);
}

#[test]
fn no_pin_means_no_warning_anywhere() {
    let w = world("pin-none", false, None);
    let status = go(&w, &source(), (false, None), "0.7.0", &now(), &|s, a| {
        commands::upgrade_status(s, a)
    });
    assert_eq!(status.out.lines, ["No upgrade is scheduled."]);
    let run = upgrade_run(&w, &source(), (true, Some(false)), "0.7.0", NOW);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert!(run.pins.is_empty());
    assert!(!run.out.lines.iter().any(|l| l.contains("pinned")));
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.7.0\n");
}

#[test]
fn keeping_the_pin_installs_it_and_leaves_spi_toml_alone() {
    let w = world("pin-keep", false, Some("0.6.0"));
    let before = text(&w.config);
    let run = upgrade_run(&w, &source(), (true, Some(false)), "0.6.0", NOW);
    assert!(run.out.ok, "{:?}", run.out.lines);
    // Asked once, with the warning and then the question.
    assert_eq!(run.pins.len(), 1);
    assert_eq!(run.pins[0][..3], PIN_WARNING);
    assert_eq!(run.pins[0][3], PIN_QUESTION);
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.6.0\n");
    assert_eq!(text(&w.config), before);
    assert_eq!(
        std::fs::read_dir(&w.dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".bak")
            })
            .count(),
        0
    );
}

#[test]
fn resetting_the_pin_installs_the_newest_and_removes_only_the_version_line() {
    let w = world("pin-reset", false, Some("0.6.0"));
    let original = text(&w.config);
    let run = upgrade_run(&w, &source(), (true, Some(true)), "0.7.0", NOW);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.7.0\n");
    // Everything else is as it was, the comments included; the air pin too.
    let expected = config_text(&w.installed, &w.dir.join("new"), None);
    assert_eq!(text(&w.config), expected);
    assert!(text(&w.config).contains("# held back until the audit\n"));
    assert!(text(&w.config).contains("version = \"1.0.2\"\n"));
    // The old file is kept.
    let backup = w.dir.join("spi.toml.1792769520.bak");
    assert_eq!(text(&backup), original);
    assert!(
        run.out.lines.contains(&format!(
            "reset: removed the `version` pin of psp-mint from {} (the old file is {})",
            w.config.display(),
            backup.display()
        )),
        "{:?}",
        run.out.lines
    );
}

#[test]
fn a_reset_that_is_not_confirmed_changes_nothing_not_even_spi_toml() {
    let w = world("pin-reset-no", false, Some("0.6.0"));
    let original = text(&w.config);
    let run = upgrade_run(&w, &source(), (false, Some(true)), "0.7.0", NOW);
    assert!(!run.out.ok);
    assert_eq!(text(&w.config), original);
    assert!(!w.dir.join("spi.toml.1792769520.bak").exists());
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert!(service_calls(&w).is_empty());
}

#[test]
fn with_no_terminal_the_pin_is_kept_and_it_says_so() {
    let w = world("pin-no-terminal", false, Some("0.6.0"));
    let run = upgrade_run(&w, &source(), (true, None), "0.6.0", NOW);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(run.out.lines[..3], PIN_WARNING);
    assert!(
        run.out.lines.contains(
            &"there is no terminal to ask on, so the pins are kept (--reset-pins installs the newest release instead)"
                .to_string()
        ),
        "{:?}",
        run.out.lines
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.6.0\n");
}

#[test]
fn keep_pins_asks_nothing_about_them_and_says_they_are_kept() {
    let w = world("pin-flag-keep", false, Some("0.6.0"));
    let case = Case {
        pins: PinPolicy::Keep,
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (true, Some(true)), "0.6.0", case);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert!(run.pins.is_empty());
    assert_eq!(run.out.lines[..3], PIN_WARNING);
    assert!(
        run.out
            .lines
            .contains(&"--keep-pins: the pins are kept".to_string())
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.6.0\n");
}

#[test]
fn reset_pins_asks_nothing_about_them_and_installs_the_newest() {
    let w = world("pin-flag-reset", false, Some("0.6.0"));
    let case = Case {
        pins: PinPolicy::Reset,
        ..NOW
    };
    let run = upgrade_run(&w, &source(), (true, Some(false)), "0.7.0", case);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert!(run.pins.is_empty());
    assert!(
        run.out.lines.contains(&format!(
            "the pin of psp-mint will be reset: the newest release is installed and its `version` line is removed from {}",
            w.config.display()
        )),
        "{:?}",
        run.out.lines
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.7.0\n");
    assert_eq!(
        text(&w.config),
        config_text(&w.installed, &w.dir.join("new"), None)
    );
}

#[test]
fn a_reset_chosen_when_scheduling_is_done_by_the_scheduled_run_and_not_before() {
    let w = world("pin-scheduled-reset", false, Some("0.6.0"));
    let original = text(&w.config);
    let case = Case {
        when: When::At(3, 0),
        pins: PinPolicy::Reset,
        dry_run: false,
    };
    let run = upgrade_run(&w, &source(), (true, None), "0.7.0", case);
    assert!(run.out.ok, "{:?}", run.out.lines);
    // Scheduling changes neither spi.toml nor the install.
    assert_eq!(text(&w.config), original);
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    let record: serde_json::Value = serde_json::from_str(&text(&pending_json(&w))).unwrap();
    assert_eq!(record["reset"], serde_json::json!(["psp-mint"]));
    // The run does it, without asking.
    let run = scheduled_run(&w, &source(), "0.7.0", &due_plus(60));
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert!(run.confirms.is_empty() && run.pins.is_empty());
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.7.0\n");
    assert_eq!(
        text(&w.config),
        config_text(&w.installed, &w.dir.join("new"), None)
    );
}

// ---- spi upgrade check ----

fn checked(w: &World, source: &dyn Source) -> commands::Checked {
    commands::upgrade_check(&w.spi, &w.exec, source)
}

fn check_source() -> FakeSource {
    FakeSource::new()
        .unreachable(Program::Bitcoind, "the release source did not answer")
        .tags(Program::Psp, &["v0.5.0", "v0.7.0", "v0.8.0-rc1", "nightly"])
        .tags(Program::Esp, &["v0.7.0"])
}

#[test]
fn check_gives_a_line_per_program_and_changes_nothing() {
    let w = world("check", false, None);
    let c = checked(&w, &check_source());
    let json = serde_json::to_value(&c.lines).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {"name": "bitcoind", "installed": "29.0.0", "spi_toml": "none", "newest": "unknown",
             "status": "unknown", "detail": "unknown (the release source did not answer)"},
            {"name": "air_server", "installed": "unknown", "spi_toml": "1.0.2", "newest": "none",
             "status": "no_releases", "detail": "no releases published upstream"},
            {"name": "psp-mint", "installed": "0.5.0", "spi_toml": "none", "newest": "0.7.0",
             "status": "upgrade_available", "detail": "upgrade available (0.7.0)"},
            {"name": "esp-mint", "installed": "0.7.0", "spi_toml": "none", "newest": "0.7.0",
             "status": "up_to_date", "detail": "up to date"}
        ])
    );
    assert!(c.warning.is_empty());
    // It only asked the programs for their version.
    assert!(
        w.exec
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.ends_with(" --version")),
        "{:?}",
        w.exec.calls.lock().unwrap()
    );
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.5.0\n");
    assert!(!w.spi.dir.join("releases").exists());
    assert!(!w.spi.dir.join("installed").exists());
}

#[test]
fn check_text_has_the_summary_line_last() {
    let w = world("check-text", false, None);
    let c = checked(&w, &check_source());
    let text = spi::release::render(&c.lines);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[2],
        "psp-mint   installed 0.5.0     spi.toml none      newest 0.7.0     upgrade available (0.7.0)"
    );
    assert_eq!(
        lines[4],
        "summary: 1 up to date, 1 upgrade available, 0 pinned, 1 unknown, 1 without upstream releases"
    );
    assert_eq!(lines.len(), 5);
}

#[test]
fn check_with_a_pin_says_pinned_and_ends_with_the_warning() {
    let w = world("check-pin", false, Some("0.5.0"));
    let c = checked(&w, &check_source());
    let psp = c.lines.iter().find(|l| l.name == "psp-mint").unwrap();
    assert_eq!(psp.status, "pinned");
    assert_eq!(psp.detail, "pinned at 0.5.0 (newest 0.7.0)");
    assert_eq!(
        c.warning,
        [
            "WARNING: these programs are pinned and are not at the newest release:",
            "  psp-mint: pinned at 0.5.0 (newest 0.7.0)",
            PIN_WARNING[2]
        ]
    );
}

#[test]
fn check_with_no_source_is_unknown_with_the_reason_and_not_a_failure() {
    let w = world("check-no-source", false, None);
    let c = checked(&w, &NoSource);
    let psp = c.lines.iter().find(|l| l.name == "psp-mint").unwrap();
    assert_eq!(psp.status, "unknown");
    assert_eq!(
        psp.detail,
        "unknown (no release source is configured in this spi)"
    );
    assert_eq!(psp.installed, "0.5.0");
}

// ---- spi restart ----

#[test]
fn restart_warns_and_asks_and_without_a_yes_restarts_nothing() {
    let w = world("restart-no", false, None);
    let run = go(&w, &NoSource, (false, None), "0.5.0", &now(), &|s, a| {
        commands::restart(s, "psp-mint", 0, a)
    });
    assert!(!run.out.ok);
    assert_eq!(run.out.lines, ["not confirmed: nothing was restarted"]);
    assert_eq!(
        run.confirms,
        [[
            "WARNING: restarting psp-mint stops it and starts it again. Nothing else is stopped. Nothing is changed unless you confirm.".to_string(),
            MINT_DOWNTIME.to_string()
        ]]
    );
    assert!(service_calls(&w).is_empty());
}

#[test]
fn restart_with_a_yes_goes_through_the_service_manager() {
    let w = world("restart-yes", false, None);
    let run = go(&w, &NoSource, (true, None), "0.5.0", &now(), &|s, a| {
        commands::restart(s, "esp-mint", 0, a)
    });
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(run.out.lines, ["esp-mint: restarted, healthy"]);
    assert_eq!(service_calls(&w), ["systemctl restart esp-mint.service"]);

    let m = world("restart-mac", true, None);
    let run = go(&m, &NoSource, (true, None), "0.5.0", &now(), &|s, a| {
        commands::restart(s, "psp-mint", 0, a)
    });
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(
        service_calls(&m),
        ["launchctl kickstart -k system/org.servicepoints.psp-mint"]
    );
}

#[test]
fn restart_is_for_the_mints_only() {
    let w = world("restart-other", false, None);
    let run = go(&w, &NoSource, (true, None), "0.5.0", &now(), &|s, a| {
        commands::restart(s, "bitcoind", 0, a)
    });
    assert!(!run.out.ok);
    assert_eq!(
        run.out.lines,
        [
            "FAIL: `bitcoind` is not a mint: `spi restart` restarts psp-mint or esp-mint (the other programs are restarted by `spi upgrade`)"
        ]
    );
    assert!(run.confirms.is_empty());
    assert!(service_calls(&w).is_empty());
}

// ---- spi snapshot ----

#[test]
fn snapshot_signals_each_mint_through_the_service_manager() {
    let w = world("snapshot-linux", false, None);
    let out = commands::snapshot(&w.spi, &w.exec);
    assert!(out.ok, "{:?}", out.lines);
    assert_eq!(
        service_calls(&w),
        [
            "systemctl kill -s USR1 --kill-whom=main psp-mint.service",
            "systemctl kill -s USR1 --kill-whom=main esp-mint.service"
        ]
    );
    assert_eq!(
        out.lines,
        [
            "psp-mint: asked for a live snapshot (SIGUSR1); it keeps serving, and its log says where the copy is",
            "esp-mint: asked for a live snapshot (SIGUSR1); it keeps serving, and its log says where the copy is"
        ]
    );
}

#[test]
fn snapshot_on_macos_uses_launchctl() {
    let w = world("snapshot-mac", true, None);
    let out = commands::snapshot(&w.spi, &w.exec);
    assert!(out.ok, "{:?}", out.lines);
    assert_eq!(
        service_calls(&w),
        [
            "launchctl kill USR1 system/org.servicepoints.psp-mint",
            "launchctl kill USR1 system/org.servicepoints.esp-mint"
        ]
    );
}

#[test]
fn a_snapshot_the_service_manager_refuses_is_a_failure_that_names_the_mint() {
    let w = world("snapshot-refused", false, None);
    let exec = fake(|_, args| {
        if args.last().map(String::as_str) == Some("psp-mint.service") {
            ran(5, "", "Unit psp-mint.service is not loaded.\n")
        } else {
            ran(0, "", "")
        }
    });
    let out = commands::snapshot(&w.spi, &exec);
    assert!(!out.ok);
    assert_eq!(
        out.lines[0],
        "FAIL: psp-mint: `systemctl kill -s USR1 --kill-whom=main psp-mint.service` failed: Unit psp-mint.service is not loaded."
    );
    // The other mint was still asked.
    assert!(out.lines[1].starts_with("esp-mint: asked for a live snapshot"));
}

// ---- a release downloaded with curl ----

#[test]
fn a_release_is_downloaded_checked_installed_and_old_downloads_are_pruned() {
    let mut w = world("curl-release", false, None);
    let (addr, _) = common::github(false);
    w.spi.release.url = format!("http://{addr}");
    w.spi.release.keep = 1;
    // Downloads from before: 0.5.0 is the version this install replaces.
    for old in ["0.1.0", "0.2.0", "0.5.0"] {
        std::fs::create_dir_all(w.dir.join("releases/psp-mint").join(old)).unwrap();
    }
    let curl = CurlSource::for_machine(&w.spi, &System, "linux", "x86_64");
    let run = upgrade_run(&w, &curl, (true, None), "0.6.0", NOW);
    assert!(run.out.ok, "{:?}", run.out.lines);
    assert_eq!(text(&w.installed.join("psp-mint")), "psp-mint 0.6.0\n");
    let releases = w.dir.join("releases/psp-mint");
    // It came from the download, not from `bin`.
    assert!(releases.join("0.6.0/psp-mint").is_file() && releases.join("0.5.0").is_dir());
    assert!(!releases.join("0.2.0").exists() && !releases.join("0.1.0").exists());
}
