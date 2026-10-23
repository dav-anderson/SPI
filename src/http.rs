//! The few HTTP calls `spi` makes: a GET that returns the status and the JSON.

use std::time::Duration;

use serde_json::Value;

pub const TIMEOUT: Duration = Duration::from_secs(5);

/// How many times a GET is tried when a connection is lost on the way.
pub const TRIES: u32 = 3;

/// `GET url`: the status code and the body as JSON (`Null` if it is not JSON).
///
/// A connection that was accepted and then closed before the answer (a mint
/// that is snapshotting or stopping closes the ones it had not read) is tried
/// again, up to [`TRIES`] times, a moment apart: a GET changes nothing. A
/// refused connection (nothing is listening) and a timeout are not tried again.
pub fn get_json(url: &str) -> Result<(u16, Value), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut last = String::new();
    for attempt in 1..=TRIES {
        match client.get(url).send() {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().map_err(|e| e.to_string())?;
                return Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)));
            }
            Err(e) if e.is_timeout() || e.is_connect() => {
                return Err(format!("cannot reach {url}: {e}"));
            }
            Err(e) => {
                last = format!("cannot reach {url}: {e}");
                if attempt < TRIES {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    Err(last)
}
