#![allow(dead_code)]

//! Driver for Quectel EG800K / BG95 / EC200A LTE modules.
//!
//! This driver follows the same structure and patterns as `simcom.rs`:
//!   - `power_on` polls AT rather than relying on fixed sleeps
//!   - `initialize_network` uses dedicated wait-helpers with real timeouts
//!   - `sleep` / `wake` manage the DTR pin in the same order as SIMCom
//!   - `send_http_request` builds header + body as one contiguous buffer
//!
//! The only intentional differences from `SimcomModule` are the Quectel-
//! specific AT commands (AT+QISEND / AT+QIRD / AT+QIOPEN etc.) and the
//! PWRKEY polarity (active-high on Quectel, active-low on SIMCom).

use esp_idf_hal::{
    gpio::{Output, PinDriver},
    uart::UartDriver,
    units::Hertz,
};
use log::info;
use anyhow::{anyhow, bail, Result};
use std::time::Duration;
use std::thread;

use crate::modem::{Modem, HttpResponse, ModemError, parse_http_response_bytes};

use chrono::NaiveDateTime;

// ---------------------------------------------------------------------------
// Driver struct
// ---------------------------------------------------------------------------

pub struct QuectelModule<'a> {
    uart:         UartDriver<'a>,
    power_pin:    PinDriver<'a, Output>,
    sleep_pin:    Option<PinDriver<'a, Output>>,
    is_connected: bool,
}

impl<'a> QuectelModule<'a> {
    pub fn new(
        uart:      UartDriver<'a>,
        power_pin: PinDriver<'a, Output>,
        sleep_pin: Option<PinDriver<'a, Output>>,
    ) -> Self {
        Self { uart, power_pin, sleep_pin, is_connected: false }
    }

    // -----------------------------------------------------------------------
    // Power management
    // -----------------------------------------------------------------------

    /// Power on the module by asserting PWRKEY high for ≥ 2 s, then polling
    /// AT until the modem responds.
    ///
    /// Quectel's PWRKEY is active-high (opposite polarity from SIMCom).
    /// Allow up to 30 s for cold starts; the typical boot time is 5–10 s.
    pub fn power_on(&mut self) -> Result<()> {
        info!("Powering on modem (PWRKEY pulse)...");

        self.power_pin.set_high()?;
        thread::sleep(Duration::from_millis(2000)); // ≥ 2 s per EG800K datasheet
        self.power_pin.set_low()?;

        self.wait_for_at_ready(Duration::from_secs(30))?;

        // Disable echo now so every subsequent response is clean.
        self.send_at_command("ATE0", "OK", Duration::from_secs(5))?;

        // Full-functionality mode.  Use the retrying variant because the
        // radio stack may not be ready immediately after the first AT response.
        self.send_at_command_until("AT+CFUN=1", "OK", Duration::from_secs(5), 5)
            .map_err(|e| anyhow!("AT+CFUN=1 failed: {}", e))?;

        thread::sleep(Duration::from_secs(2));
        info!("Modem powered on and ready");
        Ok(())
    }

    /// Poll `AT` every 500 ms until the modem replies `OK`, or `timeout` elapses.
    fn wait_for_at_ready(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for modem to become responsive (AT poll)...");
        let start   = std::time::Instant::now();
        let mut attempt = 0u32;

        while start.elapsed() < timeout {
            attempt += 1;
            match self.send_at_command("AT", "OK", Duration::from_millis(1000)) {
                Ok(_) => {
                    info!("Modem responded after {} attempt(s) ({:.1}s)",
                          attempt, start.elapsed().as_secs_f32());
                    return Ok(());
                }
                Err(_) => thread::sleep(Duration::from_millis(500)),
            }
        }

        bail!("Modem did not respond to AT within {}s", timeout.as_secs());
    }

    /// Graceful power-off via `AT+CFUN=0`, then a PWRKEY pulse.
    pub fn power_off(&mut self) -> Result<()> {
        info!("Powering off modem...");
        self.send_at_command("AT+CFUN=0", "OK", Duration::from_secs(10))?;
        thread::sleep(Duration::from_secs(1));

        self.power_pin.set_high()?;
        thread::sleep(Duration::from_millis(2000));
        self.power_pin.set_low()?;

        self.is_connected = false;
        Ok(())
    }

    /// Enter low-power sleep (`AT+QSCLK=1`), then assert DTR low.
    ///
    /// AT commands are sent **before** touching the pin so the modem
    /// accepts them while the UART is still active.
    pub fn sleep(&mut self) -> Result<()> {
        if self.sleep_pin.is_none() {
            return Err(anyhow!("Cannot put modem to sleep: no DTR connection"));
        }

        info!("Putting modem to sleep...");
        // Ignore DTR state for data mode so we can still send AT commands.
        self.send_at_command("AT&D0",    "OK", Duration::from_secs(10))?;
        // Enable slow-clock sleep; module sleeps when DTR goes low.
        self.send_at_command("AT+QSCLK=1", "OK", Duration::from_secs(10))?;

        if let Some(pin) = self.sleep_pin.as_mut() {
            pin.set_low()?;
        }
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// Wake by driving DTR high.
    pub fn wake(&mut self) -> Result<()> {
        if let Some(pin) = self.sleep_pin.as_mut() {
            info!("Waking up modem...");
            pin.set_high()?;
            thread::sleep(Duration::from_secs(1));
            Ok(())
        } else {
            Err(anyhow!("Cannot wake modem: no DTR connection"))
        }
    }

    // -----------------------------------------------------------------------
    // AT command helpers
    // -----------------------------------------------------------------------

    pub fn send_at_command_silent(
        &mut self,
        command:  &str,
        expected: &str,
        timeout:  Duration,
    ) -> Result<String> {
        self.uart.write(format!("{}\r\n", command).as_bytes())?;
        self.wait_for_response(expected, timeout, true)
    }

    pub fn send_at_command(
        &mut self,
        command:  &str,
        expected: &str,
        timeout:  Duration,
    ) -> Result<String> {
        info!("Sending: {}", command);
        self.uart.write(format!("{}\r\n", command).as_bytes())?;
        self.wait_for_response(expected, timeout, false)
    }

    /// Retry `command` up to `tries` times, sleeping 1 s between attempts.
    pub fn send_at_command_until(
        &mut self,
        command:  &str,
        expected: &str,
        timeout:  Duration,
        tries:    u32,
    ) -> Result<String, ModemError> {
        let mut remaining = tries;
        let mut last_err  = ModemError::new("no attempts made");

        while remaining > 0 {
            match self.send_at_command(command, expected, timeout) {
                Ok(resp) if resp.contains(expected) => return Ok(resp),
                Ok(resp) => {
                    last_err = ModemError::new(
                        &format!("expected `{}` but got `{}`", expected, resp));
                }
                Err(e) => {
                    info!("AT retry: {}", e);
                    last_err = ModemError::new(&e.to_string());
                }
            }
            remaining -= 1;
            if remaining > 0 {
                thread::sleep(Duration::from_secs(1));
            }
        }

        Err(last_err)
    }

    pub fn wait_for_response(
        &mut self,
        expected: &str,
        timeout:  Duration,
        silent:   bool,
    ) -> Result<String> {
        let start    = std::time::Instant::now();
        let mut resp = String::new();
        let mut buf = [0u8; 256];

        while start.elapsed() < timeout {
            match self.uart.read(&mut buf, 100) {
                Ok(n) if n > 0 => {
                    let data = String::from_utf8_lossy(&buf[..n]);
                    resp.push_str(&data);

                    if resp.contains(expected) {
                        if !silent { print!("{}", data.trim_start()); }
                        return Ok(resp);
                    }
                    if resp.contains("ERROR") {
                        bail!("AT command error: {}", resp);
                    }
                    if !silent { print!("> {}", data.trim_start()); }
                }
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }

        bail!("Timeout waiting for: {}", expected);
    }

    // -----------------------------------------------------------------------
    // UART speed helpers  (identical logic to SimcomModule)
    // -----------------------------------------------------------------------

    pub fn detect_and_set_uart_speed(&mut self, target_speed: Hertz) -> Result<()> {
        let common_rates = [115200u32, 230400, 460800, 921600];
        let target_rate: u32 = target_speed.into();

        info!("Detecting current UART speed and changing to {} baud", target_rate);

        // Quectel modules may still be booting; keep probing for up to 15 s.
        let boot_deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut detected  = None;

        'outer: while std::time::Instant::now() < boot_deadline {
            for &rate in &common_rates {
                if self.uart.change_baudrate(Hertz(rate)).is_err() { continue; }
                thread::sleep(Duration::from_millis(100));
                if self.send_at_command("AT", "OK", Duration::from_millis(500)).is_ok() {
                    info!("Found current baudrate: {} baud", rate);
                    detected = Some(rate);
                    break 'outer;
                }
            }
            info!("Modem not responding yet, retrying...");
            thread::sleep(Duration::from_millis(500));
        }

        match detected {
            Some(r) if r == target_rate => {
                info!("Already at target baudrate {} baud", target_rate);
                Ok(())
            }
            Some(_) => self.set_uart_speed(target_speed),
            None    => bail!("Could not detect current UART baudrate"),
        }
    }

    pub fn set_uart_speed(&mut self, speed: Hertz) -> Result<()> {
        let supported = [4800u32, 9600, 19200, 38400, 57600,
                         115200, 230400, 460800, 921600, 1000000];
        let rate: u32 = speed.into();

        if !supported.contains(&rate) {
            bail!("Unsupported baud rate: {}", rate);
        }

        let current: u32 = self.uart.baudrate()?.into();
        if rate == current {
            info!("UART already at {} baud", rate);
            return Ok(());
        }

        info!("Changing UART speed from {} to {} baud", current, rate);

        self.send_at_command(&format!("AT+IPR={}", rate), "OK", Duration::from_secs(5))?;
        thread::sleep(Duration::from_millis(500));
        self.uart.change_baudrate(speed)?;
        thread::sleep(Duration::from_millis(100));

        match self.test_communication() {
            Ok(())  => { info!("UART speed changed to {} baud", rate); Ok(()) }
            Err(e)  => {
                info!("New rate failed, recovering to {} baud...", current);
                let _ = self.uart.change_baudrate(Hertz(current));
                bail!("UART speed change failed: {}", e);
            }
        }
    }

    fn test_communication(&mut self) -> Result<()> {
        for attempt in 1..=3 {
            match self.send_at_command("AT", "OK", Duration::from_millis(1000)) {
                Ok(_)  => return Ok(()),
                Err(_) if attempt < 3 => {
                    info!("Communication test {} failed, retrying...", attempt);
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        }
        bail!("Communication test failed after 3 attempts");
    }

    // -----------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------

    /// Read signal quality via `AT+CSQ`, returns an approximative dBm value
    pub fn signal_quality(&mut self) -> Result<i32> {
        let resp = self.send_at_command("AT+CSQ", "OK", Duration::from_secs(5))
            .map_err(|e| anyhow!("AT+CSQ failed: {}", e))?;

        if let Some(pos) = resp.find("+CSQ:") {
            let after = resp[pos + 5..].trim_start();
            let rssi_str: String = after.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(rssi) = rssi_str.parse::<u32>() {
                if rssi != 99 {
                    let dbm = -113i32 + (rssi as i32) * 2;

                    return Ok(dbm);
                }
            }
        }

        Err(anyhow!("No signal: {}", resp))
    }

    /// Read battery voltage from `AT+CBC`.
    ///
    /// Quectel EG800K / BG95 return `+CBC: <bcs>,<bcl>,<mV>` —
    /// the voltage is in millivolts in the third comma-separated field.
    pub fn battery_voltage(&mut self) -> Result<f32> {
        let resp = self.send_at_command("AT+CBC", "OK", Duration::from_secs(30))
            .map_err(|e| anyhow!("AT+CBC failed: {}", e))?;

        if let Some(pos) = resp.find("+CBC:") {
            let after = resp[pos + 5..].trim_start();
            if let Some(mv_str) = after.split(',').nth(2) {
                let digits: String = mv_str.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if !digits.is_empty() {
                    return digits.parse::<f32>()
                        .map(|v| v / 1000.0)
                        .map_err(|e| anyhow!("Failed to parse CBC voltage '{}': {}", digits, e));
                }
            }
        }

        Err(anyhow!("No +CBC: field in response: {}", resp))
    }

    /// Read network time from `AT+QLTS`.
    pub fn network_time(&mut self) -> Result<NaiveDateTime> {
        let resp = self.send_at_command("AT+QLTS", "OK", Duration::from_millis(300))
            .map_err(|e| anyhow!("AT+QLTS failed: {}", e))?;

        if let Some(pos) = resp.find("+QLTS:") {
            let datetime_response_str = resp[pos + 8..].trim_start();
            let datetime_str = &datetime_response_str[.. 19];
            if let Ok(date) = NaiveDateTime::parse_from_str(datetime_str, "%Y/%m/%d,%H:%M:%S") {
                return Ok(date);
            } else {
                return Err(anyhow!("Could not parse date: {} ({})", resp, datetime_str));
            }
        }

        Err(anyhow!("Could not retrieve date: {}", resp))
    }

    // -----------------------------------------------------------------------
    // Network initialisation
    // -----------------------------------------------------------------------

    /// Bring up a data bearer and activate the Quectel TCP/IP context.
    ///
    /// Steps:
    ///   1. Confirm AT communication
    ///   2. Wait for SIM to report READY
    ///   3. Wait for network registration (CREG / CGREG / CEREG)
    ///   4. Confirm usable signal (CSQ ≠ 99)
    ///   5. Configure PDP context (APN)
    ///   6. Activate PDP context (AT+CGACT) and Quectel socket layer (AT+QIACT)
    ///   7. Confirm IP address assigned
    pub fn initialize_network(&mut self, apn: &str, powerup_timeout: Duration, connect_timeout: Duration) -> Result<()> {
        self.detect_and_set_uart_speed(Hertz(230400))?;

        info!("Initialising network connection...");

        // 1. Confirm AT
        self.send_at_command_until("AT", "OK", Duration::from_secs(5), (powerup_timeout.as_secs() / 5) as u32)?;

        let _ = self.send_at_command("ATE0", "OK", Duration::from_secs(2));

        // 2. SIM ready
        self.wait_for_sim_ready(Duration::from_secs(10))?;

        // 3. Network registration
        let _ = self.send_at_command("AT+CREG=1",  "OK", Duration::from_secs(1));
        let _ = self.send_at_command("AT+CGREG=1", "OK", Duration::from_secs(1));
        let _ = self.send_at_command("AT+CEREG=1", "OK", Duration::from_secs(1));
        self.wait_for_network_registration(connect_timeout)?;

        // 4. Signal quality
        self.wait_for_signal(Duration::from_secs(5))?;

        // 5. PDP context
        let pdp_cmd = format!("AT+CGDCONT=1,\"IP\",\"{}\"", apn);
        self.send_at_command(&pdp_cmd, "OK", Duration::from_secs(5))?;

        // 6. Activate bearer
        self.send_at_command("AT+CGACT=1,1", "OK", Duration::from_secs(30))?;

        match self.send_at_command("AT+QIACT=1", "OK", Duration::from_secs(30)) {
            Ok(_) => {}
            Err(e) => {
                let status = self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(10))?;
                if !status.contains("+QIACT:") {
                    return Err(e);
                }
                info!("QIACT already active, continuing...");
            }
        }

        // 7. Confirm IP
        self.send_at_command("AT+CGPADDR=1", "+CGPADDR:", Duration::from_secs(5))?;

        self.is_connected = true;
        info!("Network connection established");
        Ok(())
    }

    /// Wait up to `timeout` for `AT+CPIN?` to return `+CPIN: READY`.
    fn wait_for_sim_ready(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for SIM card to be ready...");
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            match self.send_at_command("AT+CPIN?", "OK", Duration::from_secs(3)) {
                Ok(r) if r.contains("+CPIN: READY") => {
                    info!("SIM ready ({:.1}s)", start.elapsed().as_secs_f32());
                    return Ok(());
                }
                Ok(r) if r.contains("+CPIN: SIM PIN") => bail!("SIM requires a PIN"),
                Ok(r) if r.contains("+CPIN: SIM PUK") => bail!("SIM is PUK-locked"),
                _ => thread::sleep(Duration::from_secs(1)),
            }
        }

        bail!("SIM was not ready within {}s", timeout.as_secs());
    }

    /// Poll CREG / CGREG / CEREG until any one reports registered (stat 1 or 5).
    fn wait_for_network_registration(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for network registration (up to {}s)...", timeout.as_secs());
        let start    = std::time::Instant::now();
        let mut last_log = start;

        fn is_registered(resp: &str, prefix: &str) -> bool {
            if let Some(pos) = resp.find(prefix) {
                let after = resp[pos + prefix.len()..].trim_start();
                let stat_str = if let Some(c) = after.find(',') {
                    &after[c + 1..]
                } else {
                    after
                };
                let stat: u32 = stat_str.chars()
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

            if last_log.elapsed() >= Duration::from_secs(10) {
                info!("Still waiting for registration… ({:.0}s elapsed)",
                      start.elapsed().as_secs_f32());
                last_log = std::time::Instant::now();
            }

            thread::sleep(Duration::from_secs(2));
        }

        bail!("Network registration timed out after {}s", timeout.as_secs());
    }

    /// Wait until `AT+CSQ` returns a value other than 99 (unknown signal).
    fn wait_for_signal(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for usable signal (CSQ != 99)...");
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            if let Ok(resp) = self.send_at_command("AT+CSQ", "OK", Duration::from_secs(5)) {
                if let Some(pos) = resp.find("+CSQ:") {
                    let after = resp[pos + 5..].trim_start();
                    let rssi_str: String = after.chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(rssi) = rssi_str.parse::<u32>() {
                        if rssi != 99 {
                            let dbm = -113i32 + (rssi as i32) * 2;
                            info!("Signal quality: CSQ={} (~{}dBm) ({:.1}s)",
                                rssi, dbm, start.elapsed().as_secs_f32());
                            return Ok(());
                        }
                    }
                }
            }
            thread::sleep(Duration::from_secs(2));
        }

        bail!("No usable signal (CSQ=99) within {}s", timeout.as_secs());
    }

    // -----------------------------------------------------------------------
    // TCP socket API
    // -----------------------------------------------------------------------

    pub fn open_tcp_connection(&mut self, host: &str, port: u16) -> Result<u8> {
        if !self.is_connected {
            bail!("Network not connected. Call initialize_network first.");
        }

        // Ensure the Quectel socket-layer PDP context is active.
        let qiact = self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(10))
            .unwrap_or_default();
        if !qiact.contains("+QIACT:") {
            info!("QIACT context not active, activating...");
            self.send_at_command("AT+QIACT=1", "OK", Duration::from_secs(30))
                .map_err(|e| anyhow!("Failed to activate QIACT: {}", e))?;
            let check = self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(10))?;
            if !check.contains("+QIACT:") {
                bail!("QIACT activation did not produce an IP address");
            }
        }

        // Close socket 0 only if it's open.
        let state = self.send_at_command("AT+QISTATE=0,0", "OK", Duration::from_secs(5))
            .unwrap_or_default();
        if state.contains("+QISTATE:") {
            let _ = self.send_at_command("AT+QICLOSE=0", "OK", Duration::from_secs(10));
            thread::sleep(Duration::from_millis(500));
        }

        info!("Opening TCP connection to {}:{}", host, port);
        let connect_cmd = format!("AT+QIOPEN=1,0,\"TCP\",\"{}\",{},0,0", host, port);
        self.send_at_command(&connect_cmd, "OK", Duration::from_secs(10))?;

        // Wait for async +QIOPEN URC and check error code.
        let urc = self.wait_for_response("+QIOPEN:", Duration::from_secs(10), false)?;

        if let Some(pos) = urc.find("+QIOPEN:") {
            let after = urc[pos + 8..].trim_start();
            if let Some(comma) = after.find(',') {
                let err_str: String = after[comma + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(code) = err_str.parse::<u32>() {
                    if code != 0 {
                        bail!("TCP connection failed: +QIOPEN error code {} \
                               (566=QIACT not ready)", code);
                    }
                }
            }
        }

        info!("TCP connection established (socket 0)");
        Ok(0)
    }

    pub fn close_tcp_connection(&mut self, socket_id: u8) -> Result<()> {
        info!("Closing TCP connection (socket {})", socket_id);
        let cmd = format!("AT+QICLOSE={}", socket_id);
        self.send_at_command(&cmd, "OK", Duration::from_secs(10))?;
        Ok(())
    }

    /// Send binary data.  `retries` > 0 causes a 1 s pause and one re-attempt
    /// on failure (the data is resent from scratch, not resumed mid-send).
    pub fn send_tcp_data(&mut self, socket_id: u8, data: &[u8], retries: u8) -> Result<()> {
        let cmd = format!("AT+QISEND={},{}", socket_id, data.len());
        let result = self.send_at_command(&cmd, ">", Duration::from_secs(5));
        if let Err(e) = result {
            info!("{} failed? retrying ({} left): {}", cmd, retries, e);
            if retries > 0 {
                return self.send_tcp_data(socket_id, data, retries - 1);
            } else {
                return Err(e);
            }
        }
        self.uart.write(data)?;

        if let Err(e) = self.wait_for_response("SEND OK", Duration::from_secs(10), true) {
            let _ = self.send_at_command("AT+QIGETERROR", "OK", Duration::from_secs(1));
            if retries > 0 {
                info!("send_tcp_data failed, retrying ({} left): {}", retries, e);
                thread::sleep(Duration::from_millis(1000));
                return self.send_tcp_data(socket_id, data, retries - 1);
            }
            return Err(e);
        }
        Ok(())
    }

    /// Read exactly one `AT+QIRD` chunk from the modem, returning the raw
    /// payload bytes.  Returns an empty `Vec` when the modem reports 0 bytes.
    fn read_one_qird_chunk(&mut self, socket_id: u8, max_len: usize) -> Result<Vec<u8>> {
        self.uart.write(format!("AT+QIRD={},{}\r\n", socket_id, max_len).as_bytes())?;

        let timeout = Duration::from_secs(10);
        let start   = std::time::Instant::now();
        let mut at_buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 256];

        // Collect bytes until we see the "+QIRD: <len>\r\n" header line.
        let declared_len: usize;
        loop {
            if start.elapsed() > timeout {
                bail!("Timeout waiting for +QIRD: header");
            }
            match self.uart.read(&mut tmp, 100) {
                Ok(n) if n > 0 => at_buf.extend_from_slice(&tmp[..n]),
                _ => { thread::sleep(Duration::from_millis(5)); continue; }
            }
            if let Some(pos) = at_buf.windows(2).position(|w| w == b"\r\n") {
                let line = String::from_utf8_lossy(&at_buf[..pos]);
                if let Some(after) = line.strip_prefix("+QIRD:") {
                    declared_len = after.trim().parse::<usize>()
                        .map_err(|_| anyhow!("Bad +QIRD: length: {}", after.trim()))?;
                    at_buf.drain(..pos + 2);
                    break;
                }
                // Discard echo / blank lines.
                at_buf.drain(..pos + 2);
            }
        }

        if declared_len == 0 {
            return Ok(Vec::new());
        }

        info!("QIRD chunk: declared {} bytes", declared_len);

        // `at_buf` already holds whatever arrived with the header.
        let mut payload: Vec<u8> = at_buf;
        let t2 = std::time::Instant::now();
        while payload.len() < declared_len {
            if t2.elapsed() > timeout {
                bail!("Timeout reading QIRD payload ({}/{} bytes)", payload.len(), declared_len);
            }
            match self.uart.read(&mut tmp, 100) {
                Ok(n) if n > 0 => payload.extend_from_slice(&tmp[..n]),
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }

        // Bytes beyond declared_len are the trailing \r\nOK\r\n; discard them.
        // The next call's header-scanning loop will skip any residual framing.
        payload.truncate(declared_len);
        Ok(payload)
    }

    // -----------------------------------------------------------------------
    // HTTP helpers
    // -----------------------------------------------------------------------

    pub fn send_http_request(
        &mut self,
        method:  &str,
        url:     &str,
        headers: &[(&str, &str)],
        body:    Option<&[u8]>,
    ) -> Result<HttpResponse> {
        let url = url.strip_prefix("http://").unwrap_or(url);
        let (host, path) = if let Some(p) = url.find('/') {
            (&url[..p], &url[p..])
        } else {
            (url, "/")
        };
        let (host, port) = if let Some(c) = host.find(':') {
            let port = host[c + 1..].parse::<u16>().unwrap_or(80);
            (&host[..c], port)
        } else {
            (host, 80u16)
        };

        info!("HTTP {} {}:{}{}", method, host, port, path);

        let socket_id = self.open_tcp_connection(host, port)?;

        // Build header + body as a single contiguous buffer so the declared
        // AT+QISEND length always exactly matches the bytes written.
        let mut request: Vec<u8> = Vec::new();

        request.extend_from_slice(
            format!("{} {} HTTP/1.1\r\nHost: {}\r\n", method, path, host).as_bytes()
        );

        if let Some(body) = body {
            if !headers.iter().any(|&(k, _)| k.eq_ignore_ascii_case("content-length")) {
                request.extend_from_slice(
                    format!("Content-Length: {}\r\n", body.len()).as_bytes()
                );
            }
        }

        for (key, value) in headers {
            request.extend_from_slice(format!("{}: {}\r\n", key, value).as_bytes());
        }

        request.extend_from_slice(b"Connection: close\r\n\r\n");

        if let Some(body) = body {
            request.extend_from_slice(body);
        }

        let body_len = body.map(|b| b.len()).unwrap_or(0);
        info!("HTTP request: {} bytes ({} header + {} body)",
              request.len(), request.len() - body_len, body_len);

        // Send in 1024-byte segments (safe QISEND per-call maximum).
        const MAX_SEGMENT: usize = 1024;
        let total = request.len();
        let mut sent = 0;
        for chunk in request.chunks(MAX_SEGMENT) {
            self.send_tcp_data(socket_id, chunk, 3)?;
            sent += chunk.len();
            if total > MAX_SEGMENT {
                info!("Sent {}/{} bytes ({:.1}%)",
                      sent, total, (sent as f32 / total as f32) * 100.0);
            }
        }

        // Receive the full HTTP response, tracking Content-Length to know when
        // to stop rather than waiting for a socket-close URC.
        const QIRD_CHUNK: usize = 1460;
        let mut raw: Vec<u8>        = Vec::new();
        let mut content_length: Option<usize> = None;
        let mut header_end:     Option<usize> = None;
        let mut consecutive_empty: u32 = 0;
        const MAX_EMPTY: u32 = 100; // 100 × 50 ms = 5 s stall timeout

        loop {
            let chunk = self.read_one_qird_chunk(socket_id, QIRD_CHUNK)?;
            if chunk.is_empty() {
                if let (Some(he), Some(cl)) = (header_end, content_length) {
                    if raw.len().saturating_sub(he) >= cl { break; }
                }
                consecutive_empty += 1;
                if consecutive_empty >= MAX_EMPTY {
                    bail!("Stalled: no data for 5s ({}/{} bytes)",
                          raw.len().saturating_sub(header_end.unwrap_or(0)),
                          content_length.unwrap_or(0));
                }
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            consecutive_empty = 0;
            raw.extend_from_slice(&chunk);

            // Locate header/body boundary and Content-Length once.
            if header_end.is_none() {
                if let Some(sep) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(sep + 4);
                    let hdr = String::from_utf8_lossy(&raw[..sep]);
                    for line in hdr.lines() {
                        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            if let Ok(n) = rest.trim().parse::<usize>() {
                                content_length = Some(n);
                                info!("HTTP Content-Length: {}", n);
                            }
                        }
                    }
                }
            }

            if let (Some(he), Some(cl)) = (header_end, content_length) {
                let received = raw.len().saturating_sub(he);
                if received % (64 * 1024) < QIRD_CHUNK {
                    info!("HTTP body progress: {}/{} bytes", received, cl);
                }
                if received >= cl { break; }
            }
        }

        let _ = self.close_tcp_connection(socket_id);
        info!("HTTP response received ({} total bytes)", raw.len());
        Ok(parse_http_response_bytes(&raw))
    }

    pub fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse> {
        info!("HTTP POST ({} bytes)...", body.len());
        let resp = self.send_http_request("POST", url, headers, Some(body))
            .map_err(|e| anyhow!("HTTP POST failed: {}", e))?;
        if resp.status >= 200 && resp.status < 400 {
            info!("HTTP POST successful");
        } else {
            bail!("HTTP POST failed with status {}", resp.status);
        }
        Ok(resp)
    }

    pub fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        info!("HTTP GET {}", url);
        self.send_http_request("GET", url, headers, None)
    }

    pub fn is_connected(&self) -> bool { self.is_connected }
}

// ---------------------------------------------------------------------------
// Modem trait impl
// ---------------------------------------------------------------------------

impl<'a> Modem for QuectelModule<'a> {
    fn initialize_network(&mut self, apn: &str, powerup_timeout: Duration, connect_timeout: Duration) -> Result<()> {
        self.initialize_network(apn, powerup_timeout, connect_timeout)
    }
    fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.http_post(url, body, headers)
    }
    fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.http_get(url, headers)
    }
    fn signal_quality(&mut self) -> Result<i32> {
        self.signal_quality()
    }
    fn battery_voltage(&mut self) -> Result<f32> {
        self.battery_voltage()
    }
    fn network_time(&mut self) -> Result<NaiveDateTime> {
        self.network_time()
    }
}
