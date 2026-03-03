use anyhow::{bail, Result};
use embedded_svc::http::client::Client;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::peripheral,
    http::client::{Configuration as HttpConfig, EspHttpConnection},
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use esp_idf_hal::io::Write;
use log::info;
use std::io::Read;

use crate::modem::{HttpResponse, Modem};

// ---------------------------------------------------------------------------
// WiFi connection helper (unchanged from original)
// ---------------------------------------------------------------------------

pub fn wifi(
    ssid: &str,
    pass: &str,
    modem: impl peripheral::Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,
    sysloop: EspSystemEventLoop,
) -> Result<Box<EspWifi<'static>>> {
    let mut auth_method = AuthMethod::WPA2Personal;
    if ssid.is_empty() {
        bail!("Missing WiFi name")
    }
    if pass.is_empty() {
        auth_method = AuthMethod::None;
        info!("Wifi password is empty");
    }
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
    info!("Starting wifi...");
    wifi.start()?;
    info!("Scanning...");

    let ap_infos = wifi.scan()?;

    info!("Scan found {} access points:", ap_infos.len());
    for ap in &ap_infos {
        info!("  SSID: {:?}, channel: {}, signal: {} dBm", ap.ssid, ap.channel, ap.signal_strength);
    }

    let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);
    let channel = if let Some(ours) = ours {
        info!("Found configured access point {} on channel {}", ssid, ours.channel);
        Some(ours.channel)
    } else {
        info!("Configured access point {} not found during scanning, will go with unknown channel", ssid);
        None
    };

    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().expect("Could not parse the given SSID into WiFi config"),
        password: pass.try_into().expect("Could not parse the given password into WiFi config"),
        channel,
        auth_method,
        ..Default::default()
    }))?;

    info!("Connecting wifi...");
    wifi.connect()?;
    info!("Waiting for DHCP lease...");
    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("Wifi DHCP info: {:?}", ip_info);

    Ok(Box::new(esp_wifi))
}

// ---------------------------------------------------------------------------
// WifiModem — implements the Modem trait over EspHttpConnection
// ---------------------------------------------------------------------------

pub struct WifiModem {
    // We keep _wifi alive so the connection is not dropped.
    _wifi: Box<EspWifi<'static>>,
}

impl WifiModem {
    pub fn new(wifi: Box<EspWifi<'static>>) -> Self {
        Self { _wifi: wifi }
    }

    fn send_request(
        &mut self,
        method: embedded_svc::http::Method,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse> {
        let connection = EspHttpConnection::new(&HttpConfig::default())?;
        let mut client = Client::wrap(connection);

        let mut all_headers: Vec<(&str, &str)> = headers.to_vec();

        let content_length;
        if let Some(data) = body {
            if !all_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length")) {
                content_length = format!("{}", data.len());
                all_headers.push(("Content-Length", content_length.as_str()));
            }
        }

        let mut request = client.request(method, url, &all_headers)?;

        info!("Sending {:?} HTTP request to URL '{}' through Wifi...", method, url);

        if let Some(data) = body {
            request.write_all(data)?;
            request.flush()?;
        }

        let mut response = request.submit()?;
        let status = response.status();

        // Collect response headers before consuming the body reader.
        // EspHttpConnection exposes headers via the Headers trait.
        let header_names = [
            "Content-Type",
            "Content-Length",
            "X-Firmware-Update",
            "X-Firmware-SHA256",
            "X-Firmware-Version",
        ];
        let headers: Vec<(String, String)> = header_names
            .iter()
            .filter_map(|&name| {
                response.header(name).map(|v| (name.to_string(), v.to_string()))
            })
            .collect();

        let mut body = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match response.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
                Err(e) => bail!("Error reading HTTP response body: {:?}", e),
            }
        }

        info!("Response status {}, body size: {}, headers: {:?}", status, body.len(), headers);

        Ok(HttpResponse { status, headers, body })
    }
}

impl Modem for WifiModem {
    /// For WiFi there is no APN to configure, so this is a no-op.
    /// If the connection has dropped, reconnection should be handled
    /// outside (or extended here) — good enough for a debug helper.
    fn initialize_network(&mut self, _apn: &str) -> Result<()> {
        Ok(())
    }

    fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.send_request(embedded_svc::http::Method::Post, url, headers, Some(body))
    }

    fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.send_request(embedded_svc::http::Method::Get, url, headers, None)
    }

    fn battery_voltage(&mut self) -> Result<f32> {
        bail!("battery_voltage not available on WiFi modem")
    }

    fn sleep(&mut self) -> Result<()> {
        Ok(()) // no-op: WiFi power management is handled by the IDF automatically
    }

    fn wake(&mut self) -> Result<()> {
        Ok(())
    }

    fn is_connected(&self) -> bool {
        true // if construction succeeded the IDF maintains the connection
    }
}
