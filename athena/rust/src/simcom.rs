//! SIMCom A7670E / A7670G LTE Cat-1 driver — AT command layer + PPP dial.
//!
//! # What changed from the raw-TCP version
//!
//! All TCP socket management (`AT+CIPOPEN`, `AT+CIPSEND`, `AT+CIPRXGET`, …)
//! and the hand-rolled HTTP layer have been removed.  HTTP now goes through
//! `EspHttpConnection` on top of the PPP interface brought up by `ppp.rs`.
//!
//! The driver still owns all AT command logic:
//!   - power on / off / sleep / wake
//!   - SIM ready, network registration, signal quality
//!   - PPP dial (`ATD*99***1#`) and hangup (`+++` / `ATH`)
//!   - diagnostics (battery voltage, signal quality, network time)
//!
//! # UART ownership
//!
//! The UART is held as `Arc<Mutex<UartDriver<'static>>>` so that:
//!   - the AT command methods can lock it for the duration of a command/response
//!   - the PPP RX thread (in `ppp.rs`) can lock it briefly per chunk
//!   - both never overlap: AT mode and PPP mode are mutually exclusive

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::thread;

use esp_idf_hal::{
    gpio::{Output, PinDriver},
    uart::UartDriver,
    units::Hertz,
};
use log::info;
use anyhow::{anyhow, bail, Result};
use chrono::NaiveDateTime;

use crate::modem::{ModemError, Modem};
use crate::ppp::PppConnection;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

pub struct SimcomModule<'a> {
    uart: Arc<Mutex<UartDriver<'static>>>,
    power_pin: PinDriver<'static, Output>,
    sleep_pin: Option<PinDriver<'static, Output>>,

    /// Active PPP connection.  `Some` while in data mode, `None` in AT mode.
    ppp: Option<PppConnection<'a>>,

    /// Diagnostics cached just before PPP dial (still in AT mode).
    cached_voltage:  Option<f32>,
    cached_signal:   Option<i32>,
    cached_time:     Option<NaiveDateTime>,
}

impl SimcomModule<'_> {
    pub fn new(
        uart: UartDriver<'static>,
        power_pin: PinDriver<'static, Output>,
        sleep_pin: Option<PinDriver<'static, Output>>,
    ) -> Self {
        Self {
            uart: Arc::new(Mutex::new(uart)),
            power_pin,
            sleep_pin,
            ppp: None,
            cached_voltage: None,
            cached_signal: None,
            cached_time: None,
        }
    }

    // -----------------------------------------------------------------------
    // Power management
    // -----------------------------------------------------------------------

    pub fn reboot(&mut self) -> Result<()> {
        info!("Rebooting modem");

        self.uart.lock().unwrap().change_baudrate(Hertz(460800))?;

        // If the modem is stuck in PPP data mode from a previous crash,
        // the Hayes escape sequence will return it to AT mode.
        // If it's in AT mode already, +++ is harmless (just noise on the line).
        info!("Sending escape sequence in case modem is stuck in PPP mode...");
        thread::sleep(Duration::from_millis(1000));
        {
            let uart = self.uart.lock().unwrap();
            let _ = uart.write(b"+++");
        }
        thread::sleep(Duration::from_millis(1000));
        let _ = self.send_at_command("ATH", "OK", Duration::from_secs(5));
        let _ = self.send_at_command("ATE0", "OK", Duration::from_secs(2));

        self.hangup_ppp()?;
        self.send_at_command("AT+CFUN=1,1", "OK", Duration::from_secs(5))?;
        thread::sleep(Duration::from_secs(10));
        Ok(())
    }

    pub fn power_on(&mut self) -> Result<()> {
        info!("Powering on modem (PWRKEY pulse)…");
        self.power_pin.set_low()?;
        thread::sleep(Duration::from_millis(1500));
        self.power_pin.set_high()?;

        self.wait_for_at_ready(Duration::from_secs(30))?;
        self.send_at_command("ATE0", "OK", Duration::from_secs(5))?;
        self.send_at_command_until("AT+CFUN=1", "OK", Duration::from_secs(5), 5)
            .map_err(|e| anyhow!("AT+CFUN=1 failed: {}", e))?;
        thread::sleep(Duration::from_secs(2));
        info!("Modem powered on and ready");
        Ok(())
    }

    pub fn power_off(&mut self) -> Result<()> {
        info!("Powering off modem…");
        self.hangup_ppp()?;
        self.send_at_command("AT+CPOF", "OK", Duration::from_secs(10))?;
        thread::sleep(Duration::from_secs(1));
        self.power_pin.set_low()?;
        thread::sleep(Duration::from_millis(1500));
        self.power_pin.set_high()?;
        Ok(())
    }

    pub fn sleep(&mut self) -> Result<()> {
        if self.sleep_pin.is_none() {
            return Err(anyhow!("Cannot sleep: no DTR pin"));
        }
        self.send_at_command("AT&D0",    "OK", Duration::from_secs(10))?;
        self.send_at_command("AT+CSCLK=1", "OK", Duration::from_secs(10))?;
        if let Some(pin) = self.sleep_pin.as_mut() {
            pin.set_low()?;
        }
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    pub fn wake(&mut self) -> Result<()> {
        if let Some(pin) = self.sleep_pin.as_mut() {
            info!("Waking modem…");
            pin.set_high()?;
            thread::sleep(Duration::from_secs(1));
            Ok(())
        } else {
            Err(anyhow!("Cannot wake: no DTR pin"))
        }
    }

    fn wait_for_at_ready(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for modem AT response…");
        let start = std::time::Instant::now();
        let mut attempt = 0u32;
        while start.elapsed() < timeout {
            attempt += 1;
            if self.send_at_command("AT", "OK", Duration::from_millis(1000)).is_ok() {
                info!("Modem responded after {} attempt(s)", attempt);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(500));
        }
        bail!("Modem did not respond within {}s", timeout.as_secs());
    }

    // -----------------------------------------------------------------------
    // AT command helpers
    // -----------------------------------------------------------------------

    pub fn send_at_command(
        &mut self,
        command: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<String> {
        info!("Sending: {}", command);
        let cmd_bytes = format!("{}\r\n", command);
        {
            let uart = self.uart.lock().unwrap();
            uart.write(cmd_bytes.as_bytes())?;
        }
        self.wait_for_response(expected, timeout, false)
    }

    pub fn send_at_command_silent(
        &mut self,
        command: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<String> {
        let cmd_bytes = format!("{}\r\n", command);
        {
            let uart = self.uart.lock().unwrap();
            uart.write(cmd_bytes.as_bytes())?;
        }
        self.wait_for_response(expected, timeout, true)
    }

    pub fn send_at_command_until(
        &mut self,
        command: &str,
        expected: &str,
        timeout: Duration,
        tries: u32,
    ) -> Result<String, ModemError> {
        let mut retries = tries;
        let mut last_err = ModemError::new("no attempts made");
        while retries > 0 {
            match self.send_at_command(command, expected, timeout) {
                Ok(resp) if resp.contains(expected) => return Ok(resp),
                Ok(resp) => {
                    last_err = ModemError::new(&format!(
                        "Expected `{}` but got `{}`", expected, resp
                    ));
                }
                Err(e) => {
                    last_err = ModemError::new(&e.to_string());
                }
            }
            retries -= 1;
        }
        Err(last_err)
    }

    pub fn wait_for_response(
        &mut self,
        expected: &str,
        timeout: Duration,
        silent: bool,
    ) -> Result<String> {
        let start = std::time::Instant::now();
        let mut response = String::new();
        let mut buffer = vec![0u8; 256];

        while start.elapsed() < timeout {
            let n = {
                let uart = self.uart.lock().unwrap();
                uart.read(&mut buffer, 10).unwrap_or(0)
            };
            if n > 0 {
                let data = String::from_utf8_lossy(&buffer[..n]);
                response.push_str(&data);
                if !silent {
                    eprint!("{}", data.trim_start());
                }
                if response.contains(expected) {
                    return Ok(response);
                }
                if response.contains("ERROR") {
                    bail!("AT command error: {}", response);
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        bail!("Timeout waiting for: {}", expected);
    }

    // -----------------------------------------------------------------------
    // UART speed
    // -----------------------------------------------------------------------

    pub fn detect_and_set_uart_speed(&mut self, target: Hertz) -> Result<()> {
        let target_u32: u32 = target.into();
        let rates = [115200u32, 230400, 460800, 921600];

        for &rate in &rates {
            {
                let uart = self.uart.lock().unwrap();
                if uart.change_baudrate(Hertz(rate)).is_err() {
                    continue;
                }
            }
            thread::sleep(Duration::from_millis(100));
            if self.send_at_command("AT", "OK", Duration::from_millis(500)).is_ok() {
                if rate == target_u32 {
                    info!("Already at {} baud", target_u32);
                    return Ok(());
                }
                return self.set_uart_speed(target);
            }
        }
        bail!("Could not detect UART baudrate");
    }

    fn set_uart_speed(&mut self, speed: Hertz) -> Result<()> {
        let rate: u32 = speed.into();
        let cmd = format!("AT+IPR={}", rate);
        self.send_at_command(&cmd, "OK", Duration::from_secs(5))?;
        thread::sleep(Duration::from_millis(500));
        {
            let uart = self.uart.lock().unwrap();
            uart.change_baudrate(speed)?;
        }
        thread::sleep(Duration::from_millis(100));
        self.send_at_command("AT", "OK", Duration::from_millis(1000))
            .map_err(|e| anyhow!("UART speed change verification failed: {}", e))?;
        info!("UART speed set to {} baud", rate);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Network / SIM readiness
    // -----------------------------------------------------------------------

    fn wait_for_sim_ready(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for SIM card…");
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            match self.send_at_command("AT+CPIN?", "OK", Duration::from_secs(3)) {
                Ok(r) if r.contains("+CPIN: READY") => {
                    info!("SIM ready ({:.1}s)", start.elapsed().as_secs_f32());
                    return Ok(());
                }
                Ok(r) if r.contains("SIM PIN") => bail!("SIM requires PIN"),
                Ok(r) if r.contains("SIM PUK") => bail!("SIM is PUK-locked"),
                _ => thread::sleep(Duration::from_secs(1)),
            }
        }
        bail!("SIM not ready within {}s", timeout.as_secs());
    }

    fn wait_for_network_registration(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for network registration…");
        let start = std::time::Instant::now();

        fn is_registered(resp: &str, prefix: &str) -> bool {
            if let Some(pos) = resp.find(prefix) {
                let after = resp[pos + prefix.len()..].trim_start();
                let stat_str = if let Some(c) = after.find(',') {
                    &after[c + 1..]
                } else {
                    after
                };
                let stat: u32 = stat_str
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
                return stat == 1 || stat == 5;
            }
            false
        }

        while start.elapsed() < timeout {
            let creg  = self.send_at_command("AT+CREG?",  "OK", Duration::from_secs(5)).unwrap_or_default();
            let cgreg = self.send_at_command("AT+CGREG?", "OK", Duration::from_secs(5)).unwrap_or_default();
            let cereg = self.send_at_command("AT+CEREG?", "OK", Duration::from_secs(5)).unwrap_or_default();

            if is_registered(&creg, "+CREG:")
                || is_registered(&cgreg, "+CGREG:")
                || is_registered(&cereg, "+CEREG:")
            {
                info!("Network registered ({:.1}s)", start.elapsed().as_secs_f32());
                return Ok(());
            }
            thread::sleep(Duration::from_secs(2));
        }
        bail!("Network registration timed out after {}s", timeout.as_secs());
    }

    fn wait_for_signal(&mut self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Ok(resp) = self.send_at_command("AT+CSQ", "OK", Duration::from_secs(5)) {
                if let Some(pos) = resp.find("+CSQ:") {
                    let rssi: u32 = resp[pos + 5..]
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse()
                        .unwrap_or(99);
                    if rssi != 99 {
                        return Ok(());
                    }
                }
            }
            thread::sleep(Duration::from_secs(2));
        }
        bail!("No usable signal within {}s", timeout.as_secs());
    }

    // -----------------------------------------------------------------------
    // Diagnostics (AT mode only)
    // -----------------------------------------------------------------------

    pub fn signal_quality_at(&mut self) -> Result<i32> {
        let resp = self.send_at_command("AT+CSQ", "OK", Duration::from_secs(5))
            .map_err(|e| anyhow!("AT+CSQ failed: {}", e))?;
        if let Some(pos) = resp.find("+CSQ:") {
            let rssi: u32 = resp[pos + 5..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(99);
            if rssi != 99 {
                return Ok(-113 + rssi as i32 * 2);
            }
        }
        Err(anyhow!("No signal: {}", resp))
    }

    pub fn battery_voltage_at(&mut self) -> Result<f32> {
        let resp = self.send_at_command("AT+CBC", "OK", Duration::from_secs(30))
            .map_err(|e| anyhow!("AT+CBC failed: {}", e))?;
        if let Some(pos) = resp.find("+CBC:") {
            let value_str: String = resp[pos + 5..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !value_str.is_empty() {
                return value_str.parse::<f32>()
                    .map_err(|e| anyhow!("Could not parse CBC voltage '{}': {}", value_str, e));
            }
        }
        Err(anyhow!("No +CBC: in response: {}", resp))
    }

    pub fn network_time_at(&mut self) -> Result<NaiveDateTime> {
        let resp = self.send_at_command("AT+CCLK?", "OK", Duration::from_secs(3))
            .map_err(|e| anyhow!("AT+CCLK? failed: {}", e))?;
        if let Some(pos) = resp.find("+CCLK:") {
            let after = resp[pos + 7..].trim_start().trim_matches('"');
            // "24/04/08,13:48:06+08" → take first 17 chars
            let dt = &after[..17.min(after.len())];
            return NaiveDateTime::parse_from_str(dt, "%y/%m/%d,%H:%M:%S")
                .map_err(|e| anyhow!("CCLK parse error for '{}': {}", dt, e));
        }
        Err(anyhow!("Could not retrieve network time"))
    }

    // -----------------------------------------------------------------------
    // PPP dial / hangup
    // -----------------------------------------------------------------------

    /// Switch the modem to PPP data mode, then bring up the PPP netif.
    ///
    /// On success, `self.ppp` is `Some` and HTTP via `EspHttpConnection` works.
    pub fn dial_ppp(&mut self) -> Result<()> {
        if self.ppp.is_some() {
            info!("PPP already active");
            return Ok(());
        }

        info!("Dialling PPP (ATD*99***1#)…");
        let cmd = b"ATD*99***1#\r\n";
        {
            let uart = self.uart.lock().unwrap();
            uart.write(cmd)?;
        }

        // Wait for "CONNECT" — do NOT use send_at_command because after CONNECT
        // the modem is no longer in AT mode.
        let start = std::time::Instant::now();
        let mut response = String::new();
        let mut buf = vec![0u8; 64];
        loop {
            if start.elapsed() > Duration::from_secs(30) {
                bail!("PPP dial timeout: no CONNECT");
            }
            let n = {
                let uart = self.uart.lock().unwrap();
                uart.read(&mut buf, 100).unwrap_or(0)
            };
            if n > 0 {
                response.push_str(&String::from_utf8_lossy(&buf[..n]));
                if response.contains("CONNECT") {
                    info!("Modem in PPP data mode");
                    break;
                }
                if response.contains("NO CARRIER") || response.contains("ERROR") {
                    bail!("PPP dial failed: {}", response);
                }
            }
        }

        // Tiny pause so the modem fully enters data mode before we start
        // feeding it PPP frames.
        thread::sleep(Duration::from_millis(100));

        let ppp = PppConnection::new(
            Arc::clone(&self.uart),
            Duration::from_secs(60),
        )?;
        self.ppp = Some(ppp);
        info!("PPP data mode active");
        Ok(())
    }

    /// Return to AT command mode by sending the Hayes escape sequence.
    ///
    /// Safe to call even if PPP is not active.
    pub fn hangup_ppp(&mut self) -> Result<()> {
        if self.ppp.is_none() {
            //return Ok(());
        }

        // Drop the PppConnection first — this stops the RX thread and destroys
        // the netif, which sends a LCP Terminate to the modem.
        self.ppp = None;

        // Give the LCP teardown a moment, then send the Hayes escape.
        thread::sleep(Duration::from_millis(1000));

        // The standard PPP escape: 1 s guard, "+++", 1 s guard.
        // We write directly to avoid send_at_command's response parsing.
        thread::sleep(Duration::from_millis(1000));
        {
            let uart = self.uart.lock().unwrap();
            uart.write(b"+++")?;
        }
        thread::sleep(Duration::from_millis(1000));

        // Hang up.
        let _ = self.send_at_command("ATH", "OK", Duration::from_secs(5));
        info!("Returned to AT command mode");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Modem trait impl
// ---------------------------------------------------------------------------

impl<'a> Modem for SimcomModule<'a> {
    fn initialize_network(
        &mut self,
        apn: &str,
        powerup_timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<()> {
        if self.ppp.is_some() {
            return Ok(());
        }

        self.detect_and_set_uart_speed(Hertz(460800))?;

        self.send_at_command_until(
            "AT", "OK",
            Duration::from_secs(5),
            (powerup_timeout.as_secs() / 5) as u32,
        )?;
        let _ = self.send_at_command("ATE0", "OK", Duration::from_secs(2));

        self.wait_for_sim_ready(Duration::from_secs(10))?;

        // Enable registration URCs.
        self.send_at_command("AT+CREG=1", "OK", Duration::from_secs(5))?;
        self.send_at_command("AT+CGREG=1", "OK", Duration::from_secs(5))?;
        self.send_at_command("AT+CEREG=1", "OK", Duration::from_secs(5))?;

        self.wait_for_network_registration(connect_timeout)?;
        self.wait_for_signal(Duration::from_secs(5))?;

        // Set APN.
        let pdp_cmd = format!("AT+CGDCONT=1,\"IP\",\"{}\"", apn);
        self.send_at_command(&pdp_cmd, "OK", Duration::from_secs(5))?;

        self.cached_voltage = self.battery_voltage_at().ok();
        self.cached_signal = self.signal_quality_at().ok();
        self.cached_time = self.network_time_at().ok();

        info!(
            "Diagnostics cached: voltage={:?} signal={:?} time={:?}",
            self.cached_voltage, self.cached_signal, self.cached_time,
        );

        self.dial_ppp()
    }

    fn reboot(&mut self) -> Result<()> {
        self.reboot()
    }

    fn power_off(&mut self) -> Result<()> {
        self.power_off()
    }

    fn battery_voltage(&mut self) -> Result<f32> {
        self.cached_voltage.ok_or_else(|| anyhow!("Battery voltage not yet sampled"))
    }

    fn signal_quality(&mut self) -> Result<i32> {
        self.cached_signal.ok_or_else(|| anyhow!("Signal quality not yet sampled"))
    }

    fn network_time(&mut self) -> Result<NaiveDateTime> {
        self.cached_time.ok_or_else(|| anyhow!("Network time not yet sampled"))
    }
}
