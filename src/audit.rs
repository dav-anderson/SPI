//! `spi audit`: what the PSP and ESP mints owe, in their own units, in PSP and
//! in USD. Built from the two mints' own `GET /audit` replies.
//!
//! - **PSP outstanding**: psp-mint's `outstanding`, in PSP (whole PSP).
//! - **ESP**: esp-mint's `outstanding`, in ESP units (`ESP_PER_PSP` of them to
//!   the PSP), itemized as matured and unmatured when esp-mint reports
//!   `outstanding_matured` and `outstanding_unmatured` (esp-mint 0.5.0 and
//!   later). Both count at full value: neither is discounted for waiting. The
//!   two add up to `outstanding`, and a reply where they do not is refused,
//!   unless esp-mint says it has requests it has not finished
//!   (`requests_unfinished`), which it has not counted yet.
//! - **PSP paid to the SPI** (psp-mint's `paid`, with `burned` and `reissued`
//!   when it reports them): what spends of PSP paid to the SPI itself. With
//!   burning off all of it is reissued to the SPI's wallet; with it on, what is
//!   above the threshold is burned, and `burned` is the SPI's revenue in PSP.
//! - **The SPI's wallet** (`ecash-wallet balance`): what the SPI holds, shown
//!   beside what the mints owe. It is not subtracted from anything. If the
//!   wallet cannot be read the audit still prints and says so.
//! - **FSP is not counted.** It is backed by ESP the FSP mint holds, which is
//!   already in the ESP figure; counting it again would count it twice.
//!
//! All sums are exact integers (`u128`), checked: a value that would overflow
//! is an error, never a wrapped number. PSP and USD are printed as exact
//! decimals worked out from the integers.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;
use sp_wire::units::ESP_PER_PSP;

/// PSP to dollars: 10 PSP = 1 USD.
///
/// MUST MATCH THE PAYOUT RATE. The payout side (outside these repos) pays
/// dollars at this rate; if the two differ, this report states a liability that
/// is not the one that will be paid. It is the only place `spi` holds the rate;
/// psp-mint and esp-mint's own unit constants (`ESP_PER_PSP`, and the ESP
/// mint's `min_psp` of "10 PSP = $1") say the same thing.
pub const PSP_PER_USD: u128 = 10;

const ESP_PER_PSP_U: u128 = ESP_PER_PSP as u128;

/// A figure in its own unit, and the same value in PSP and USD (exact decimals).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Line {
    /// `PSP` or `ESP`.
    pub unit: String,
    /// In `unit`, as a decimal string (a number JSON readers could round).
    pub units: String,
    pub psp: String,
    pub usd: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub psp_outstanding: Line,
    /// `None` when esp-mint does not report the split (see `warnings`).
    pub esp_matured: Option<Line>,
    pub esp_unmatured: Option<Line>,
    /// esp-mint's outstanding total, as it reported it.
    pub esp_total: Line,
    /// PSP outstanding plus all ESP outstanding.
    pub total: Line,
    /// What psp-mint reports of the PSP it was paid, when it reports it.
    pub psp_revenue: Option<Revenue>,
    /// What the SPI's own wallet holds, when it could be read.
    pub wallet: Option<WalletReport>,
    pub warnings: Vec<String>,
}

/// psp-mint's `paid`, `burned` and `reissued`, in PSP.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Revenue {
    pub paid: String,
    /// `None` from a psp-mint that does not report it.
    pub burned: Option<String>,
    pub reissued: Option<String>,
}

/// The SPI wallet's balance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WalletReport {
    pub psp: Line,
    pub esp: Line,
}

/// What the two replies hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Figures {
    pub psp: u128,
    pub esp_total: u128,
    pub esp_matured: Option<u128>,
    pub esp_unmatured: Option<u128>,
    /// esp-mint's `requests_unfinished`: 0 when it does not report it.
    pub esp_unfinished: u128,
}

/// A JSON number or a decimal string, as a whole number.
pub fn whole(v: &Value) -> Option<u128> {
    match v {
        Value::Number(n) => n.as_u64().map(u128::from),
        Value::String(s) => s.trim().parse::<u128>().ok(),
        _ => None,
    }
}

fn field(v: &Value, key: &str, who: &str) -> Result<u128, String> {
    let f = v
        .get(key)
        .ok_or_else(|| format!("{who} /audit has no `{key}`"))?;
    whole(f).ok_or_else(|| format!("{who} /audit `{key}` is not a whole number: {f}"))
}

fn optional(v: &Value, key: &str, who: &str) -> Result<Option<u128>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(f) => whole(f)
            .map(Some)
            .ok_or_else(|| format!("{who} /audit `{key}` is not a whole number: {f}")),
    }
}

/// The figures from the two `GET /audit` replies.
pub fn figures(psp: &Value, esp: &Value) -> Result<Figures, String> {
    let psp_out = field(psp, "outstanding", "psp-mint")?;
    let esp_total = field(esp, "outstanding", "esp-mint")?;
    let matured = optional(esp, "outstanding_matured", "esp-mint")?;
    let unmatured = optional(esp, "outstanding_unmatured", "esp-mint")?;
    let unfinished = optional(esp, "requests_unfinished", "esp-mint")?.unwrap_or(0);
    match (matured, unmatured) {
        (Some(m), Some(u)) => {
            let sum = m
                .checked_add(u)
                .ok_or("esp-mint matured plus unmatured overflows")?;
            // esp-mint adds a request to the split when it finishes it, and
            // says how many it has not. With none, the two must add up.
            if sum != esp_total && unfinished == 0 {
                return Err(format!(
                    "esp-mint /audit does not add up: matured {m} + unmatured {u} = {sum}, but outstanding is {esp_total}"
                ));
            }
        }
        (None, None) => {}
        _ => {
            return Err(
                "esp-mint /audit has only one of outstanding_matured and outstanding_unmatured"
                    .into(),
            );
        }
    }
    Ok(Figures {
        psp: psp_out,
        esp_total,
        esp_matured: matured,
        esp_unmatured: unmatured,
        esp_unfinished: unfinished,
    })
}

/// What psp-mint's `GET /audit` says it was paid. `None` when it has no `paid`.
pub fn revenue(psp: &Value) -> Result<Option<Revenue>, String> {
    let Some(paid) = optional(psp, "paid", "psp-mint")? else {
        return Ok(None);
    };
    Ok(Some(Revenue {
        paid: paid.to_string(),
        burned: optional(psp, "burned", "psp-mint")?.map(|n| n.to_string()),
        reissued: optional(psp, "reissued", "psp-mint")?.map(|n| n.to_string()),
    }))
}

/// The wallet's balance as the report shows it.
pub fn wallet_report(psp: u128, esp: u128) -> Result<WalletReport, String> {
    let scaled = psp
        .checked_mul(ESP_PER_PSP_U)
        .ok_or("the wallet's PSP is too large to add up (it overflows 128 bits)")?;
    Ok(WalletReport {
        psp: line("PSP", psp, scaled),
        esp: line("ESP", esp, esp),
    })
}

/// `scaled` (ESP units) as a decimal with `digits` places after the point.
fn decimal(scaled: u128, per: u128) -> String {
    let width = per.to_string().len() - 1;
    format!("{}.{:0width$}", scaled / per, scaled % per)
}

fn line(unit: &str, units: u128, scaled: u128) -> Line {
    Line {
        unit: unit.to_string(),
        units: units.to_string(),
        psp: decimal(scaled, ESP_PER_PSP_U),
        // scaled is in ESP units; ESP_PER_PSP * PSP_PER_USD of them to the dollar.
        usd: decimal(scaled, ESP_PER_PSP_U * PSP_PER_USD),
    }
}

/// The report. Errors on overflow instead of wrapping.
pub fn report(f: &Figures) -> Result<Report, String> {
    let overflow = || "a figure is too large to add up (it overflows 128 bits)".to_string();
    let psp_scaled = f.psp.checked_mul(ESP_PER_PSP_U).ok_or_else(overflow)?;
    let total_scaled = psp_scaled.checked_add(f.esp_total).ok_or_else(overflow)?;
    let mut warnings = Vec::new();
    if f.esp_matured.is_none() {
        warnings.push(
            "esp-mint does not report outstanding_matured / outstanding_unmatured, so ESP is shown as one figure; esp-mint 0.5.0 and later report the split"
                .to_string(),
        );
    }
    if f.esp_unfinished > 0 {
        warnings.push(format!(
            "esp-mint has {} unfinished request(s): the split does not count what they have done so far, so its parts may not add up to the total until they finish",
            f.esp_unfinished
        ));
    }
    Ok(Report {
        psp_revenue: None,
        wallet: None,
        psp_outstanding: line("PSP", f.psp, psp_scaled),
        esp_matured: f.esp_matured.map(|m| line("ESP", m, m)),
        esp_unmatured: f.esp_unmatured.map(|u| line("ESP", u, u)),
        esp_total: line("ESP", f.esp_total, f.esp_total),
        total: line("PSP", total_scaled / ESP_PER_PSP_U, total_scaled),
        warnings,
    })
}

/// Text for a person.
pub fn render(r: &Report, network: &str) -> String {
    let mut out = String::new();
    let row = |out: &mut String, name: &str, l: &Line| {
        let _ = writeln!(
            out,
            "  {name:<16} {:>28} {:<3}  = {:>30} PSP  = ${}",
            l.units, l.unit, l.psp, l.usd
        );
    };
    let _ = writeln!(
        out,
        "spi audit on {network} (10 PSP = 1 USD; FSP not counted)"
    );
    row(&mut out, "PSP outstanding", &r.psp_outstanding);
    match (&r.esp_matured, &r.esp_unmatured) {
        (Some(m), Some(u)) => {
            row(&mut out, "ESP matured", m);
            row(&mut out, "ESP unmatured", u);
        }
        _ => {
            let _ = writeln!(out, "  ESP matured / unmatured: not reported by esp-mint");
        }
    }
    row(&mut out, "ESP total", &r.esp_total);
    row(&mut out, "TOTAL", &r.total);
    if let Some(v) = &r.psp_revenue {
        let _ = writeln!(
            out,
            "  PSP paid to the SPI: {} PSP, of which burned (revenue) {}, reissued to the SPI wallet {}",
            v.paid,
            v.burned.as_deref().unwrap_or("not reported"),
            v.reissued.as_deref().unwrap_or("not reported"),
        );
    }
    if let Some(w) = &r.wallet {
        row(&mut out, "SPI wallet PSP", &w.psp);
        row(&mut out, "SPI wallet ESP", &w.esp);
    }
    for w in &r.warnings {
        let _ = writeln!(out, "WARNING: {w}");
    }
    out
}
