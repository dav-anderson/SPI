//! The audit's arithmetic, on figures and on mint replies.

use serde_json::json;
use spi::audit::{self, Figures};

fn figs(psp: u128, total: u128, matured: Option<u128>, unmatured: Option<u128>) -> Figures {
    Figures {
        psp,
        esp_total: total,
        esp_matured: matured,
        esp_unmatured: unmatured,
        esp_unsplit: 0,
        esp_unfinished: 0,
    }
}

#[test]
fn ten_psp_is_one_dollar_and_a_psp_is_1e11_esp() {
    // 10 PSP, 5 PSP of ESP matured (5 * 10^11), 2.5 PSP unmatured.
    let r = audit::report(&figs(
        10,
        750_000_000_000,
        Some(500_000_000_000),
        Some(250_000_000_000),
    ))
    .unwrap();
    assert_eq!(r.psp_outstanding.units, "10");
    assert_eq!(r.psp_outstanding.psp, "10.00000000000");
    assert_eq!(r.psp_outstanding.usd, "1.000000000000");
    let m = r.esp_matured.as_ref().unwrap();
    assert_eq!((m.unit.as_str(), m.units.as_str()), ("ESP", "500000000000"));
    assert_eq!(m.psp, "5.00000000000");
    assert_eq!(m.usd, "0.500000000000");
    let u = r.esp_unmatured.as_ref().unwrap();
    assert_eq!(u.psp, "2.50000000000");
    assert_eq!(u.usd, "0.250000000000");
    // Both ESP figures count at full value, and the total is the sum.
    assert_eq!(r.esp_total.psp, "7.50000000000");
    assert_eq!(r.total.psp, "17.50000000000");
    assert_eq!(r.total.usd, "1.750000000000");
    // Nothing is unsplit, and nothing is warned about.
    assert_eq!(r.esp_unsplit.as_ref().unwrap().units, "0");
    assert!(r.warnings.is_empty());
}

#[test]
fn a_single_esp_unit_is_kept_exactly() {
    let r = audit::report(&figs(0, 1, Some(1), Some(0))).unwrap();
    assert_eq!(r.esp_total.psp, "0.00000000001");
    assert_eq!(r.esp_total.usd, "0.000000000001");
    assert_eq!(r.total.psp, "0.00000000001");
}

#[test]
fn large_values_do_not_overflow_or_lose_digits() {
    let psp = u128::from(u64::MAX);
    let esp = u128::from(u64::MAX) * 1_000;
    let r = audit::report(&figs(psp, esp, Some(esp), Some(0))).unwrap();
    assert_eq!(r.psp_outstanding.units, "18446744073709551615");
    assert_eq!(r.psp_outstanding.psp, "18446744073709551615.00000000000");
    assert_eq!(r.psp_outstanding.usd, "1844674407370955161.500000000000");
    // 10^3 * u64::MAX ESP units = u64::MAX / 10^8 PSP.
    assert_eq!(r.esp_total.psp, "184467440737.09551615000");
    // 18446744073709551615 + 184467440737.09551615 PSP (the whole parts add to this)
    assert_eq!(r.total.psp, "18446744258176992352.09551615000");
}

#[test]
fn a_figure_too_large_to_add_is_an_error_not_a_wrapped_number() {
    // psp * 10^11 would not fit in 128 bits.
    assert!(audit::report(&figs(u128::MAX, 0, None, None)).is_err());
    assert!(audit::report(&figs(u128::MAX / 100_000_000_000, u128::MAX, None, None)).is_err());
    // Matured plus unmatured overflowing is refused when the replies are read.
    let psp = json!({"outstanding": 1});
    let esp = json!({
        "outstanding": "1",
        "outstanding_matured": "340282366920938463463374607431768211455",
        "outstanding_unmatured": "1"
    });
    assert!(audit::figures(&psp, &esp).is_err());
}

#[test]
fn replies_are_read_as_numbers_or_strings() {
    let psp = json!({"issued": 100, "redeemed": 40, "outstanding": 60, "network": "regtest"});
    let esp = json!({"issued": "7", "redeemed": 2, "outstanding": 5});
    let f = audit::figures(&psp, &esp).unwrap();
    assert_eq!((f.psp, f.esp_total), (60, 5));
    assert_eq!((f.esp_matured, f.esp_unmatured), (None, None));
    // No split: one figure, and a warning that says why.
    let r = audit::report(&f).unwrap();
    assert!(r.esp_matured.is_none() && r.esp_unmatured.is_none());
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings[0].contains("outstanding_matured"));
    assert!(r.esp_unsplit.is_none());
    let text = audit::render(&r, "regtest");
    assert!(text.contains("not reported by esp-mint") && text.contains("WARNING"));
}

#[test]
fn a_split_that_does_not_add_up_or_is_half_there_is_refused() {
    let psp = json!({"outstanding": 1});
    let wrong = json!({"outstanding": 10, "outstanding_matured": 4, "outstanding_unmatured": 5});
    assert!(
        audit::figures(&psp, &wrong)
            .unwrap_err()
            .contains("does not add up")
    );
    let half = json!({"outstanding": 10, "outstanding_matured": 10});
    assert!(audit::figures(&psp, &half).is_err());
    let missing = json!({"issued": 1});
    assert!(audit::figures(&missing, &json!({"outstanding": 1})).is_err());
    let bad = json!({"outstanding": "ten"});
    assert!(audit::figures(&psp, &bad).is_err());
}

#[test]
fn the_split_with_nothing_unsplit_has_no_warning_and_prints_the_zero() {
    let psp = json!({"outstanding": 0});
    let esp = json!({
        "outstanding": "7",
        "outstanding_matured": "3",
        "outstanding_unmatured": "4",
        "outstanding_unsplit": "0",
        "requests_unfinished": 0
    });
    let f = audit::figures(&psp, &esp).unwrap();
    assert_eq!(
        (
            f.esp_matured,
            f.esp_unmatured,
            f.esp_unsplit,
            f.esp_unfinished
        ),
        (Some(3), Some(4), 0, 0)
    );
    let r = audit::report(&f).unwrap();
    assert!(r.warnings.is_empty());
    let text = audit::render(&r, "regtest");
    assert!(
        text.contains("ESP unsplit") && !text.contains("WARNING"),
        "{text}"
    );
}

#[test]
fn esp_issued_before_the_split_is_shown_as_unsplit_and_warned_about_by_its_size() {
    let psp = json!({"outstanding": 0});
    // 5 PSP of ESP outstanding: 1 matured, 1.5 unmatured, 2.5 from before esp-mint kept the split.
    let esp = json!({
        "outstanding": 500_000_000_000u64,
        "outstanding_matured": 100_000_000_000u64,
        "outstanding_unmatured": 150_000_000_000u64,
        "outstanding_unsplit": 250_000_000_000u64,
        "requests_unfinished": 0
    });
    let f = audit::figures(&psp, &esp).unwrap();
    assert_eq!(f.esp_unsplit, 250_000_000_000);
    let r = audit::report(&f).unwrap();
    let x = r.esp_unsplit.as_ref().unwrap();
    assert_eq!(
        (x.units.as_str(), x.psp.as_str()),
        ("250000000000", "2.50000000000")
    );
    // The total is still esp-mint's outstanding, counted once.
    assert_eq!(r.esp_total.psp, "5.00000000000");
    assert_eq!(r.total.psp, "5.00000000000");
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings[0].contains("250000000000 ESP (2.50000000000 PSP)"));
    assert!(r.warnings[0].contains("before esp-mint kept the split"));
    let text = audit::render(&r, "regtest");
    assert!(
        text.contains("ESP unsplit") && text.contains("WARNING"),
        "{text}"
    );
}

#[test]
fn the_unsplit_part_is_counted_in_the_sum_that_must_equal_the_total() {
    let psp = json!({"outstanding": 1});
    // matured + unmatured alone is short of the total, and unsplit covers it
    let ok = json!({
        "outstanding": 10,
        "outstanding_matured": 4,
        "outstanding_unmatured": 5,
        "outstanding_unsplit": 1
    });
    assert!(audit::figures(&psp, &ok).is_ok());
    // and a wrong unsplit does not
    let wrong = json!({
        "outstanding": 10,
        "outstanding_matured": 4,
        "outstanding_unmatured": 5,
        "outstanding_unsplit": 2
    });
    let e = audit::figures(&psp, &wrong).unwrap_err();
    assert!(
        e.contains("does not add up") && e.contains("unsplit 2"),
        "{e}"
    );
    // an esp-mint that has only the old two parts is read as before
    let old = json!({"outstanding": 9, "outstanding_matured": 4, "outstanding_unmatured": 5});
    assert_eq!(audit::figures(&psp, &old).unwrap().esp_unsplit, 0);
    // unsplit without the split is a half reply
    let odd = json!({"outstanding": 9, "outstanding_unsplit": 9});
    assert!(audit::figures(&psp, &odd).is_err());
    // overflow is an error, not a wrapped number
    let big = json!({
        "outstanding": 1,
        "outstanding_matured": "1",
        "outstanding_unmatured": "1",
        "outstanding_unsplit": "340282366920938463463374607431768211455"
    });
    assert!(audit::figures(&psp, &big).is_err());
}

#[test]
fn unfinished_requests_excuse_a_gap_and_are_warned_about() {
    let psp = json!({"outstanding": 1});
    // A transfer has burned its inputs and is not counted yet: the parts are
    // above the total for a moment.
    let esp = json!({
        "outstanding": 8,
        "outstanding_matured": 4,
        "outstanding_unmatured": 5,
        "outstanding_unsplit": 0,
        "requests_unfinished": 1
    });
    let f = audit::figures(&psp, &esp).unwrap();
    assert_eq!(f.esp_unfinished, 1);
    let r = audit::report(&f).unwrap();
    // The total is esp-mint's `outstanding`, not the parts.
    assert_eq!(r.esp_total.units, "8");
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings[0].contains("1 unfinished request"));
    // With none unfinished the same figures are refused.
    let none = json!({
        "outstanding": 8,
        "outstanding_matured": 4,
        "outstanding_unmatured": 5,
        "outstanding_unsplit": 0,
        "requests_unfinished": 0
    });
    assert!(audit::figures(&psp, &none).is_err());
}

#[test]
fn the_rate_is_one_named_constant() {
    assert_eq!(audit::PSP_PER_USD, 10);
}

#[test]
fn the_report_prints_every_line_in_its_unit_psp_and_usd() {
    let r = audit::report(&figs(10, 10, Some(4), Some(6))).unwrap();
    let text = audit::render(&r, "regtest");
    for needle in [
        "PSP outstanding",
        "ESP matured",
        "ESP unmatured",
        "ESP total",
        "TOTAL",
        "FSP not counted",
        "$1.000000000000",
    ] {
        assert!(text.contains(needle), "{needle} not in:\n{text}");
    }
}
