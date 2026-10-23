//! The audit's arithmetic, on figures and on mint replies.

use serde_json::json;
use spi::audit::{self, Figures};

fn figs(psp: u128, total: u128, matured: Option<u128>, unmatured: Option<u128>) -> Figures {
    Figures {
        psp,
        esp_total: total,
        esp_matured: matured,
        esp_unmatured: unmatured,
        esp_unfinished: 0,
    }
}

#[test]
fn ten_psp_is_one_dollar_and_a_psp_is_1e11_esp() {
    // The rate is one named constant.
    assert_eq!(audit::PSP_PER_USD, 10);
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
    // Nothing is warned about.
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
    let overflow = "a figure is too large to add up (it overflows 128 bits)";
    assert_eq!(
        audit::report(&figs(u128::MAX, 0, None, None)).unwrap_err(),
        overflow
    );
    assert_eq!(
        audit::report(&figs(u128::MAX / 100_000_000_000, u128::MAX, None, None)).unwrap_err(),
        overflow
    );
    // Matured plus unmatured overflowing is refused when the replies are read.
    let psp = json!({"outstanding": 1});
    let esp = json!({
        "outstanding": "1",
        "outstanding_matured": "340282366920938463463374607431768211455",
        "outstanding_unmatured": "1"
    });
    assert_eq!(
        audit::figures(&psp, &esp).unwrap_err(),
        "esp-mint matured plus unmatured overflows"
    );
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
    let text = audit::render(&r, "regtest");
    assert!(text.contains("not reported by esp-mint") && text.contains("WARNING"));
}

#[test]
fn a_split_that_does_not_add_up_or_is_half_there_is_refused() {
    let psp = json!({"outstanding": 1});
    let wrong = json!({"outstanding": 10, "outstanding_matured": 4, "outstanding_unmatured": 5});
    assert_eq!(
        audit::figures(&psp, &wrong).unwrap_err(),
        "esp-mint /audit does not add up: matured 4 + unmatured 5 = 9, but outstanding is 10"
    );
    let half = json!({"outstanding": 10, "outstanding_matured": 10});
    assert_eq!(
        audit::figures(&psp, &half).unwrap_err(),
        "esp-mint /audit has only one of outstanding_matured and outstanding_unmatured"
    );
    let missing = json!({"issued": 1});
    assert_eq!(
        audit::figures(&missing, &json!({"outstanding": 1})).unwrap_err(),
        "psp-mint /audit has no `outstanding`"
    );
    let bad = json!({"outstanding": "ten"});
    assert_eq!(
        audit::figures(&psp, &bad).unwrap_err(),
        "esp-mint /audit `outstanding` is not a whole number: \"ten\""
    );
}

#[test]
fn the_split_prints_its_zero_lines_with_no_warning() {
    let psp = json!({"outstanding": 0});
    let esp = json!({
        "outstanding": "7",
        "outstanding_matured": "7",
        "outstanding_unmatured": "0",
        "requests_unfinished": 0
    });
    let f = audit::figures(&psp, &esp).unwrap();
    assert_eq!(
        (f.esp_matured, f.esp_unmatured, f.esp_unfinished),
        (Some(7), Some(0), 0)
    );
    let r = audit::report(&f).unwrap();
    assert!(r.warnings.is_empty());
    let text = audit::render(&r, "regtest");
    assert!(!text.contains("WARNING"), "{text}");
    // The line for the part that is zero is there, not left out.
    assert!(
        text.contains(
            "  ESP unmatured                               0 ESP  =                  0.00000000000 PSP  = $0.000000000000\n"
        ),
        "{text}"
    );
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
        "requests_unfinished": 0
    });
    assert_eq!(
        audit::figures(&psp, &none).unwrap_err(),
        "esp-mint /audit does not add up: matured 4 + unmatured 5 = 9, but outstanding is 8"
    );
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
