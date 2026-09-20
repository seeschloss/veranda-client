//! Shared modem abstractions: `Modem` trait and `HttpResponse`.
//!
//! Both `SimcomModule` and `QuectelModule` implement `Modem`, allowing
//! `main.rs` to hold a `Box<dyn Modem>` regardless of the hardware fitted.
//!
//! HTTP responses are now constructed directly from `EspHttpConnection`
//! responses rather than from raw TCP bytes, so `parse_http_response_bytes`
//! has been removed.  The `HttpResponse` type is kept because `main.rs`
//! pattern-matches on it and reads headers by name.

use chrono::NaiveDateTime;
use std::time::Duration;
use log::info;
use anyhow::{anyhow, bail, Result};

use esp_idf_svc::http::client::Configuration as HttpConfig;

// ---------------------------------------------------------------------------
// Shared error type
// ---------------------------------------------------------------------------

/// A simple string-carrying error used by retry helpers in both modem drivers.
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
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

// ---------------------------------------------------------------------------
// Modem trait
// ---------------------------------------------------------------------------

pub trait Modem {
    fn initialize_network(&mut self, apn: &str, powerup_timeout: Duration, connect_timeout: Duration) -> Result<()>;

    fn http_post(
        &mut self,
        url: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        use embedded_svc::http::client::Client;
        use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};

        info!("HTTP POST {} ({} bytes)", url, body.len());

        let conn = EspHttpConnection::new(&HttpConfig {
            timeout: Some(Duration::from_secs(120)),
            ..HttpConfig::default()
        }).map_err(|e| anyhow!("EspHttpConnection::new failed: {:?}", e))?;
        let mut client = Client::wrap(conn);

        let content_length = body.len().to_string();
        let mut full_headers: Vec<(&str, &str)> = Vec::with_capacity(headers.len() + 1);
        full_headers.push(("Content-Length", &content_length));
        full_headers.extend_from_slice(headers);

        let mut req = client
            .post(url, &full_headers)
            .map_err(|e| anyhow!("POST request creation failed: {:?}", e))?;

        req.write(body)
            .map_err(|e| anyhow!("Writing POST body failed: {}", e))?;

        req.flush()
            .map_err(|e| anyhow!("Flushing POST request failed: {}", e))?;

        let mut resp = req
            .submit()
            .map_err(|e| anyhow!("POST submit failed: {:?}", e))?;

        let result = collect_http_response(&mut resp)?;

        if result.status < 200 || result.status >= 400 {
            bail!("HTTP POST returned status {}: {:?}", result.status, result.headers);
        } else {
            info!("HTTP POST successful, status {}: {:?}", result.status, result.headers);
        }
        Ok(result)
    }

    fn http_get(
        &mut self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        use embedded_svc::http::client::Client;
        use embedded_svc::http::Method;
        use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};

        info!("HTTP GET {}", url);

        let conn = EspHttpConnection::new(&HttpConfig::default())
            .map_err(|e| anyhow!("EspHttpConnection::new failed: {:?}", e))?;
        let mut client = Client::wrap(conn);

        let mut req = client
            .request(Method::Get, url, headers)
            .map_err(|e| anyhow!("GET request creation failed: {:?}", e))?;

        req.flush()
            .map_err(|e| anyhow!("Flushing GET request failed: {}", e))?;

        let mut resp = req
            .submit()
            .map_err(|e| anyhow!("GET submit failed: {:?}", e))?;

        let result = collect_http_response(&mut resp)?;

        if result.status < 200 || result.status >= 400 {
            bail!("HTTP GET returned status {}", result.status);
        } else {
            info!("HTTP GET successful, status {}: {:?}", result.status, result.headers);
        }
        Ok(result)
    }

    /// Returns the last signal quality reading (dBm).
    /// Sampled during `initialize_network`; cached until the next call.
    fn battery_voltage(&mut self) -> Result<f32>;

    /// Returns the last signal quality reading (dBm).
    fn signal_quality(&mut self) -> Result<i32>;

    /// Returns the network time sampled during `initialize_network`.
    fn network_time(&mut self) -> Result<NaiveDateTime>;

    fn reboot(&mut self) -> Result<()>;
    fn power_off(&mut self) -> Result<()>;
}

// ---------------------------------------------------------------------------
// HTTP helper: build an HttpResponse from an EspHttpConnection response
// ---------------------------------------------------------------------------

/// Read the status, headers, and body from an `embedded-svc` HTTP response
/// into our `HttpResponse` type.
///
/// Called by both `SimcomModule` and `QuectelModule` after `req.submit()`.
pub fn collect_http_response<C>(
    resp: &mut embedded_svc::http::client::Response<C>,
) -> Result<HttpResponse>
where
    C: embedded_svc::http::client::Connection,
{
    let status = resp.status();

    // Fetch only the headers main.rs actually reads by name.
    // embedded-svc Response has header(name) but no headers() iterator.
    let known_headers = [
        "X-Jpeg-Quality",
        "X-Brightness-Threshold",
        "X-Sleep-Minutes",
        "X-Firmware-Update",
        "X-Firmware-SHA256",
        "X-Firmware-Version",
        "Content-Type",
        "Content-Length",
        "Connection",
    ];

    let headers: Vec<(String, String)> = known_headers
        .iter()
        .filter_map(|&name| {
            resp.header(name)
                .map(|value| (name.to_string(), value.to_string()))
        })
    .collect();

    let mut body = Vec::new();
    let mut chunk = vec![0u8; 4096];
    loop {
        match resp.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(anyhow::anyhow!("Failed to read HTTP response body: {}", e)),
        }
    }

    Ok(HttpResponse { status, headers, body })
}
