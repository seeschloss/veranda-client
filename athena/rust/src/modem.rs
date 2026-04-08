//! Shared modem abstractions: `Modem` trait, `HttpResponse`, and helpers.
//!
//! Both `SimcomModule` and `QuectelModule` implement `Modem`, allowing
//! `main.rs` to hold a `Box<dyn Modem>` regardless of the hardware fitted.

use anyhow::Result;
use chrono::NaiveDateTime;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared error type  ← three design options described below
// ---------------------------------------------------------------------------

/// A simple string-carrying error used by the retry helper
/// `send_at_command_until` in both modem drivers.
///
/// **Design options — choose whichever suits the project style:**
///
/// ### Option A — keep module-local types (current / no shared import)
/// Each driver defines its own `SimcomError` / `QuectelError` locally.
/// The types are structurally identical but separate.
///
/// ### Option B — use `ModemError` from this module  ← CURRENTLY ACTIVE
/// Both drivers do `use crate::modem::ModemError;`
/// Callers that match on the error type only need one import.
///
/// ### Option C — use `anyhow::Error` throughout (simplest)
/// Remove custom error types entirely; change `send_at_command_until`
/// to return `anyhow::Result<String>`.  Works well because both drivers
/// already use `anyhow` for every other error path.
#[derive(Debug)]
pub struct ModemError {
    details: String,
}

impl ModemError {
    pub fn new(msg: &str) -> Self {
        Self { details: msg.to_owned() }
    }
}

impl std::fmt::Display for ModemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl std::error::Error for ModemError {}

// ---------------------------------------------------------------------------
// HTTP response type
// ---------------------------------------------------------------------------

pub struct HttpResponse {
    pub status:  u16,
    pub headers: Vec<(String, String)>,
    pub body:    Vec<u8>,
}

impl HttpResponse {
    /// Look up a response header by name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

// ---------------------------------------------------------------------------
// Modem trait
// ---------------------------------------------------------------------------

pub trait Modem {
    fn initialize_network(&mut self, apn: &str, powerup_timeout: Duration, connect_timeout: Duration) -> Result<()>;
    fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse>;
    fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse>;
    fn battery_voltage(&mut self) -> Result<f32>;
    fn signal_quality(&mut self) -> Result<i32>;
    fn network_time(&mut self) -> Result<NaiveDateTime>;
}

// ---------------------------------------------------------------------------
// HTTP response parsers
// ---------------------------------------------------------------------------

/// Parse a raw HTTP response byte slice into an `HttpResponse`.
///
/// This is binary-safe: the body is stored as `Vec<u8>` with no UTF-8
/// decoding, so firmware images and other binary payloads are never mangled.
/// Both `SimcomModule` and `QuectelModule` should use this function.
pub fn parse_http_response_bytes(raw: &[u8]) -> HttpResponse {
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n");
    let (header_bytes, body) = if let Some(pos) = sep {
        (&raw[..pos], raw[pos + 4..].to_vec())
    } else {
        (raw, Vec::new())
    };

    let header_str = String::from_utf8_lossy(header_bytes);
    let mut lines   = header_str.lines();

    let status = lines.next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();

    HttpResponse { status, headers, body }
}

