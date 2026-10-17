//! The few HTTP calls `spi` makes: a GET that returns the status and the JSON.

use std::time::Duration;

use serde_json::Value;

pub const TIMEOUT: Duration = Duration::from_secs(5);

/// `GET url`: the status code and the body as JSON (`Null` if it is not JSON).
pub fn get_json(url: &str) -> Result<(u16, Value), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("cannot reach {url}: {e}"))?;
    let status = response.status().as_u16();
    let body = response.text().map_err(|e| e.to_string())?;
    Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
}
