#![allow(dead_code)]

use esp_idf_hal::{
    uart::UartDriver,
    units::Hertz,
    gpio::{OutputPin, PinDriver, Output},
};
use log::info;
use anyhow::{anyhow, Result, bail};
use std::time::Duration;
use std::thread;

// ---------------------------------------------------------------------------
// Error type (mirrors QuectelError for API compatibility)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SimcomError {
    details: String,
}

impl SimcomError {
    pub fn new(msg: &str) -> Self {
        Self { details: msg.to_owned() }
    }
}

impl std::fmt::Display for SimcomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.details)
    }
}

impl std::error::Error for SimcomError {
    fn description(&self) -> &str {
        &self.details
    }
}

// ---------------------------------------------------------------------------
// Main driver struct
// ---------------------------------------------------------------------------

/// Driver for SIMCom A7670E / A7670G LTE Cat-1 modules.
///
/// Pin mapping
/// -----------
/// * `power_pin`  – connected to the module's PWRKEY line (active-low pulse
///                  to power on/off; pulled up internally by the module).
/// * `sleep_pin`  – optional DTR line used to wake the module from sleep
///                  (`AT+CSCLK=1` mode).
///
/// This struct is a drop-in replacement for `QuectelModule` – it exposes the
/// same public methods with identical signatures.
pub struct SimcomModule<'a> {
    uart: UartDriver<'a>,
    power_pin: PinDriver<'a, Output>,
    sleep_pin: Option<PinDriver<'a, Output>>,
    is_connected: bool,
}

impl<'a> SimcomModule<'a> {
    pub fn new(
        uart: UartDriver<'a>,
        power_pin: PinDriver<'a, Output>,
        sleep_pin: Option<PinDriver<'a, Output>>,
    ) -> Self {
        Self {
            uart,
            power_pin,
            sleep_pin,
            is_connected: false,
        }
    }

    // -----------------------------------------------------------------------
    // Power management
    // -----------------------------------------------------------------------

    /// Power on the A7670 by asserting PWRKEY low for ≥1 s then releasing it.
    ///
    /// The A7670 hardware reference recommends pulling PWRKEY low for at least
    /// 1 second; the module will begin its boot sequence once the pin is
    /// released.  Rather than waiting a fixed delay we poll `AT` in a loop
    /// until the modem acknowledges, then set full-functionality mode.
    pub fn power_on(&mut self) -> Result<()> {
        info!("Powering on modem (PWRKEY pulse)...");

        // PWRKEY is active-low on SIMCom modules.
        // Drive the pin low to start the power-on pulse.
        self.power_pin.set_low()?;
        thread::sleep(Duration::from_millis(1500)); // ≥1 s per datasheet
        self.power_pin.set_high()?;                  // release

        // Poll AT until the modem responds, with a generous overall timeout.
        // The A7670 typically boots in 3–8 s; allow up to 30 s for cold starts
        // on a low battery or slow power rail.
        self.wait_for_at_ready(Duration::from_secs(30))?;

        // Disable echo now so every subsequent response is predictable.
        self.send_at_command("ATE0", "OK", Duration::from_secs(5))?;

        // Set to full-functionality mode (RF on, SIM powered).
        // Use send_at_command_until because the module can still be
        // initialising its radio stack and return ERROR for a short window
        // right after it first responds to AT.
        self.send_at_command_until("AT+CFUN=1", "OK", Duration::from_secs(5), 5)
            .map_err(|e| anyhow!("AT+CFUN=1 failed: {}", e))?;

        // Brief settle time for the radio to come up after CFUN=1.
        thread::sleep(Duration::from_secs(2));
        info!("Modem powered on and ready");
        Ok(())
    }

    /// Poll `AT` every 500 ms until the modem replies `OK`, or `timeout`
    /// elapses.  Used after power-on and after reset to avoid fixed sleeps.
    fn wait_for_at_ready(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for modem to become responsive (AT poll)...");
        let start = std::time::Instant::now();
        let mut attempt = 0u32;

        while start.elapsed() < timeout {
            attempt += 1;
            match self.send_at_command("AT", "OK", Duration::from_millis(1000)) {
                Ok(_) => {
                    info!("Modem responded to AT after {} attempt(s) ({:.1}s)",
                        attempt, start.elapsed().as_secs_f32());
                    return Ok(());
                }
                Err(_) => {
                    // Modem still booting — wait before retrying.
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }

        bail!("Modem did not respond to AT within {}s", timeout.as_secs());
    }

    /// Power off the A7670 cleanly via AT command followed by a PWRKEY pulse.
    pub fn power_off(&mut self) -> Result<()> {
        info!("Powering off modem...");
        // Graceful shutdown via AT command first.
        self.send_at_command("AT+CPOF", "OK", Duration::from_secs(10))?;
        thread::sleep(Duration::from_secs(1));

        // Hardware power-off pulse (same timing as power-on).
        self.power_pin.set_low()?;
        thread::sleep(Duration::from_millis(1500));
        self.power_pin.set_high()?;

        self.is_connected = false;
        Ok(())
    }

    /// Enter low-power sleep mode using `AT+CSCLK=1`.
    ///
    /// Requires DTR (`sleep_pin`) to be wired; the pin is driven low to
    /// allow the module to sleep once the AT command is accepted.
    pub fn sleep(&mut self) -> Result<()> {
        if self.sleep_pin.is_none() {
            return Err(anyhow!("Cannot put modem to sleep: no DTR connection"));
        }

        info!("Putting modem to sleep...");
        // Send AT commands while no borrow of sleep_pin is live.
        // Ignore DTR for normal data mode so we can still send AT commands.
        self.send_at_command("AT&D0", "OK", Duration::from_secs(10))?;
        // Enable slow-clock sleep: module sleeps when DTR is low.
        self.send_at_command("AT+CSCLK=1", "OK", Duration::from_secs(10))?;

        // Now borrow sleep_pin to assert DTR low → module enters sleep.
        if let Some(sleep_pin) = self.sleep_pin.as_mut() {
            sleep_pin.set_low()?;
        }
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// Wake the module from sleep by driving DTR high.
    pub fn wake(&mut self) -> Result<()> {
        if let Some(sleep_pin) = self.sleep_pin.as_mut() {
            info!("Waking up modem...");
            sleep_pin.set_high()?;
            thread::sleep(Duration::from_secs(1));
            Ok(())
        } else {
            Err(anyhow!("Cannot wake up modem: no DTR connection"))
        }
    }

    // -----------------------------------------------------------------------
    // AT command helpers (identical API to QuectelModule)
    // -----------------------------------------------------------------------

    pub fn send_at_command_silent(
        &mut self,
        command: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<String> {
        self.uart.write(format!("{}\r\n", command).as_bytes())?;
        self.wait_for_response(expected, timeout, true)
    }

    pub fn send_at_command(
        &mut self,
        command: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<String> {
        info!("Sending: {}", command);
        self.uart.write(format!("{}\r\n", command).as_bytes())?;
        self.wait_for_response(expected, timeout, false)
    }

    pub fn send_at_command_until(
        &mut self,
        command: &str,
        expected: &str,
        timeout: Duration,
        tries: i32,
    ) -> Result<String, SimcomError> {
        let mut retries = tries;
        let mut result = Err(SimcomError::new("no attempts made"));

        while retries > 0 {
            match self.send_at_command(command, expected, timeout) {
                Ok(response) => {
                    if response.contains(expected) {
                        return Ok(response);
                    } else {
                        result = Err(SimcomError::new(
                            &format!("Expected `{}` but got `{}`", expected, response),
                        ));
                    }
                }
                Err(e) => {
                    info!("SIM card not ready ({})", e);
                    result = Err(SimcomError::new(&e.to_string()));
                }
            }
            retries -= 1;
        }

        result
    }

    pub fn wait_for_response(
        &mut self,
        expected: &str,
        timeout: Duration,
        silent: bool,
    ) -> Result<String> {
        let start = std::time::Instant::now();
        let mut response = String::new();
        let mut buffer = [0u8; 256];

        while start.elapsed() < timeout {
            match self.uart.read(&mut buffer, 100) {
                Ok(len) if len > 0 => {
                    let data = String::from_utf8_lossy(&buffer[..len]);
                    response.push_str(&data);

                    if response.contains(expected) {
                        if !silent {
                            print!("{}", data.trim_start());
                        }
                        return Ok(response);
                    }

                    if response.contains("ERROR") {
                        bail!("AT command error: {}", response);
                    }

                    if !silent {
                        print!("> {}", data.trim_start());
                    }
                }
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }

        bail!("Timeout waiting for: {}", expected);
    }

    // -----------------------------------------------------------------------
    // UART speed helpers
    // -----------------------------------------------------------------------

    pub fn detect_and_set_uart_speed(&mut self, target_speed: Hertz) -> Result<()> {
        let common_rates = vec![115200, 230400, 460800, 921600];
        let target_rate: u32 = target_speed.into();

        info!("Detecting current UART speed and changing to {} baud", target_rate);

        let mut current_detected_rate = None;

        for &rate in &common_rates {
            info!("Testing communication at {} baud", rate);
            if let Ok(_) = self.uart.change_baudrate(Hertz(rate)) {
                thread::sleep(Duration::from_millis(100));
                if let Ok(_) = self.send_at_command("AT", "OK", Duration::from_millis(500)) {
                    info!("Found current baudrate: {} baud", rate);
                    current_detected_rate = Some(rate);
                    break;
                }
            }
        }

        match current_detected_rate {
            Some(current_rate) if current_rate == target_rate => {
                info!("Already at target baudrate {} baud", target_rate);
                Ok(())
            }
            Some(_) => self.set_uart_speed(target_speed),
            None => bail!("Could not detect current UART baudrate"),
        }
    }

    pub fn set_uart_speed(&mut self, speed: Hertz) -> Result<()> {
        // A7670 supported rates (same as EG800K).
        let supported_rates = vec![
            4800u32, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000,
        ];
        let rate_u32: u32 = speed.into();

        if !supported_rates.contains(&rate_u32) {
            bail!("Unsupported baud rate: {}", rate_u32);
        }

        let current_rate: u32 = self.uart.baudrate()?.into();
        if rate_u32 == current_rate {
            info!("UART already at {} baud", rate_u32);
            return Ok(());
        }

        info!("Changing UART speed from {} to {} baud", current_rate, rate_u32);

        // AT+IPR is identical on SIMCom.
        let cmd = format!("AT+IPR={}", rate_u32);
        self.send_at_command(&cmd, "OK", Duration::from_secs(5))?;
        thread::sleep(Duration::from_millis(500));

        self.uart.change_baudrate(speed)?;
        thread::sleep(Duration::from_millis(100));

        match self.test_communication() {
            Ok(()) => {
                info!("UART speed successfully changed to {} baud", rate_u32);
                Ok(())
            }
            Err(e) => {
                info!("Failed to communicate at new rate, attempting recovery...");
                let original_rate = Hertz(current_rate);
                if let Err(_) = self.uart.change_baudrate(original_rate) {
                    bail!("Failed to recover original UART speed");
                }
                bail!("UART speed change failed: {}", e);
            }
        }
    }

    fn test_communication(&mut self) -> Result<()> {
        for attempt in 1..=3 {
            match self.send_at_command("AT", "OK", Duration::from_millis(1000)) {
                Ok(_) => return Ok(()),
                Err(_) if attempt < 3 => {
                    info!("Communication test attempt {} failed, retrying...", attempt);
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

    /// Read battery voltage via `AT+CBC`.
    ///
    /// The A7670 returns `+CBC: <voltage>V` where voltage is already in volts
    /// (e.g. `+CBC: 3.894V`), unlike the EG800K which returns millivolts as
    /// the third comma-separated field of `+CBC: <bcs>,<bcl>,<mV>`.
    pub fn battery_voltage(&mut self) -> Result<f32> {
        let response = self.send_at_command("AT+CBC", "OK", Duration::from_secs(30))
            .map_err(|e| anyhow!("AT+CBC command failure: {}", e))?;

        // Locate "+CBC:" then parse the float that follows.
        // The response looks like "+CBC: 3.894V\r\n" — extract only the
        // numeric characters (digits and '.') immediately after the prefix,
        // ignoring any trailing unit or whitespace.
        if let Some(cbc_pos) = response.find("+CBC:") {
            let after = response[cbc_pos + 5..].trim_start();
            let value_str: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !value_str.is_empty() {
                return value_str.parse::<f32>()
                    .map_err(|e| anyhow!("Failed to parse CBC voltage '{}': {}", value_str, e));
            }
        }

        Err(anyhow!("No +CBC: field in response: {}", response))
    }

    // -----------------------------------------------------------------------
    // Network initialisation
    // -----------------------------------------------------------------------

    /// Bring up a data bearer and open the TCP/IP stack.
    ///
    /// SIMCom A7670 TCP/IP model:
    ///   1. Configure PDP context with `AT+CGDCONT` (standard, same as Quectel)
    ///   2. Activate context with `AT+CGACT`
    ///   3. Open the network service with `AT+NETOPEN` (replaces Quectel's
    ///      implicit activation via `AT+QIACT`)
    pub fn initialize_network(&mut self, apn: &str) -> Result<()> {
        self.detect_and_set_uart_speed(Hertz(460800))?;

        info!("Initializing network connection...");

        // ── 1. Confirm the modem is talking to us ───────────────────────────
        self.send_at_command_until("AT", "OK", Duration::from_secs(5), 30)?;

        // Disable echo (may already be off from power_on, but belt-and-braces).
        let _ = self.send_at_command("ATE0", "OK", Duration::from_secs(2));

        // ── 2. Wait for SIM card to be ready ────────────────────────────────
        // The SIM can take several seconds to initialise after CFUN=1.
        self.wait_for_sim_ready(Duration::from_secs(30))?;

        // ── 3. Wait for network registration ────────────────────────────────
        // Enable unsolicited registration URCs, then poll until the modem
        // reports it is registered on the home network or roaming.
        self.send_at_command("AT+CREG=1",  "OK", Duration::from_secs(5))?;
        self.send_at_command("AT+CGREG=1", "OK", Duration::from_secs(5))?;
        self.send_at_command("AT+CEREG=1", "OK", Duration::from_secs(5))?;

        self.wait_for_network_registration(Duration::from_secs(90))?;

        // ── 4. Confirm signal quality is usable (CSQ != 99) ─────────────────
        self.wait_for_signal(Duration::from_secs(30))?;

        // ── 5. Configure PDP context ─────────────────────────────────────────
        // Do NOT manually call AT+CGACT here: on the A7670 the bearer is
        // managed entirely by AT+NETOPEN / AT+NETCLOSE.  Calling CGACT=0
        // underneath an open network session triggers "+CGEV: NW PDN DEACT"
        // and "+CIPEVENT: NETWORK CLOSED UNEXPECTEDLY" which tears everything
        // down.  Just set the APN and let NETOPEN handle activation.
        let pdp_cmd = format!("AT+CGDCONT=1,\"IP\",\"{}\"", apn);
        self.send_at_command(&pdp_cmd, "OK", Duration::from_secs(5))?;

        // ── 6. Open the network service ──────────────────────────────────────
        // AT+NETOPEN activates the internal TCP/IP stack; equivalent to
        // AT+QIACT on Quectel but with a different URC (+NETOPEN:).
        // If the network is already open the module replies with
        // "+IP ERROR: Network is already opened" instead of "+NETOPEN: 0",
        // which is fine — either way we have what we need.
        match self.send_at_command("AT+NETOPEN", "+NETOPEN:", Duration::from_secs(30)) {
            Ok(_) => {},
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("already opened") {
                    info!("Network was already open, continuing...");
                } else {
                    return Err(e);
                }
            }
        }

        // ── 7. Confirm an IP address was assigned ────────────────────────────
        self.send_at_command("AT+CGPADDR=1", "+CGPADDR:", Duration::from_secs(5))?;

        self.is_connected = true;
        info!("Network connection established");
        Ok(())
    }

    /// Wait up to `timeout` for the SIM card to report `+CPIN: READY`.
    ///
    /// On a cold start the SIM can take several seconds to power up and
    /// complete its initialisation even after the modem is already responding
    /// to AT commands.
    fn wait_for_sim_ready(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for SIM card to be ready...");
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            match self.send_at_command("AT+CPIN?", "OK", Duration::from_secs(3)) {
                Ok(resp) if resp.contains("+CPIN: READY") => {
                    info!("SIM card ready ({:.1}s)", start.elapsed().as_secs_f32());
                    return Ok(());
                }
                Ok(resp) if resp.contains("+CPIN: SIM PIN") => {
                    bail!("SIM card requires a PIN – cannot proceed");
                }
                Ok(resp) if resp.contains("+CPIN: SIM PUK") => {
                    bail!("SIM card is PUK-locked – cannot proceed");
                }
                Ok(_) | Err(_) => {
                    // Not ready yet – wait and retry.
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }

        bail!("SIM card was not ready within {}s", timeout.as_secs());
    }

    /// Poll registration status until the modem is registered (home or
    /// roaming) on at least one of CREG / CGREG / CEREG, or `timeout` elapses.
    ///
    /// Registered states per 3GPP TS 27.007:
    ///   1 = registered, home network
    ///   5 = registered, roaming
    fn wait_for_network_registration(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for network registration (up to {}s)...", timeout.as_secs());
        let start = std::time::Instant::now();

        // Helper: extract the numeric stat field from "+CREG: <n>,<stat>" or
        // "+CREG: <stat>" and return true if it indicates registered.
        fn is_registered(response: &str, prefix: &str) -> bool {
            if let Some(pos) = response.find(prefix) {
                let after = response[pos + prefix.len()..].trim_start();
                // The stat value is either the first token ("+CREG: 1") or the
                // second comma-separated token ("+CREG: 0,1").
                let stat_str = if let Some(comma) = after.find(',') {
                    &after[comma + 1..]
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

        let mut last_stat_log = start;

        while start.elapsed() < timeout {
            // Query all three registration commands; any one registering is enough.
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

            // Log progress every 10 s so the user can see something is happening.
            if last_stat_log.elapsed() >= Duration::from_secs(10) {
                info!("Still waiting for registration… ({:.0}s elapsed)",
                    start.elapsed().as_secs_f32());
                last_stat_log = std::time::Instant::now();
            }

            thread::sleep(Duration::from_secs(2));
        }

        bail!("Network registration timed out after {}s", timeout.as_secs());
    }

    /// Wait until `AT+CSQ` returns a signal-quality value other than 99
    /// (which means "unknown / not detectable").
    fn wait_for_signal(&mut self, timeout: Duration) -> Result<()> {
        info!("Waiting for usable signal (CSQ != 99)...");
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            if let Ok(resp) = self.send_at_command("AT+CSQ", "OK", Duration::from_secs(5)) {
                // Response: "+CSQ: <rssi>,<ber>"  where rssi 0-31 are valid,
                // 99 means not known / not detectable.
                if let Some(pos) = resp.find("+CSQ:") {
                    let after = resp[pos + 5..].trim_start();
                    let rssi_str: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(rssi) = rssi_str.parse::<u32>() {
                        if rssi != 99 {
                            // Convert to rough dBm for a useful log message.
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

    /// Open a TCP connection and return the socket ID (0).
    ///
    /// SIMCom uses `AT+CIPOPEN` where Quectel uses `AT+QIOPEN`.
    /// The URC confirming the connection is `+CIPOPEN:` instead of `+QIOPEN:`.
    pub fn open_tcp_connection(&mut self, host: &str, port: u16) -> Result<u8> {
        if !self.is_connected {
            bail!("Network not connected. Call initialize_network first.");
        }

        // Query open sockets. The response lists each open socket as a
        // "+CIPOPEN: <n>" line and ends with OK.
        // Only close socket 0 if it actually appears in the listing — sending
        // AT+CIPCLOSE for a socket that isn't open returns error code 4 and
        // leaves the stack in a state where AT+CIPOPEN is then rejected.
        let socket_already_open = match self.send_at_command("AT+CIPOPEN?", "OK", Duration::from_secs(5)) {
            Ok(resp) => resp.contains("+CIPOPEN: 0"),
            Err(_) => false,
        };

        if socket_already_open {
            let _ = self.send_at_command("AT+CIPCLOSE=0", "OK", Duration::from_secs(5));
            thread::sleep(Duration::from_millis(500));
        }

        info!("Opening TCP connection to {}:{}", host, port);

        // AT+CIPOPEN=<socket>,<type>,<host>,<port>
        let connect_cmd = format!("AT+CIPOPEN=0,\"TCP\",\"{}\",{}", host, port);
        self.send_at_command(&connect_cmd, "OK", Duration::from_secs(10))?;

        // Wait for the async connection confirmation URC.
        // Format: +CIPOPEN: <socket>,<err>  where err=0 means success.
        let urc = self.wait_for_response("+CIPOPEN:", Duration::from_secs(150), false)?;

        // Parse the error code out of "+CIPOPEN: 0,<err>".
        if let Some(urc_pos) = urc.find("+CIPOPEN:") {
            let after = urc[urc_pos + 9..].trim_start();
            // after looks like "0,1\r\n" – the error code is after the comma.
            if let Some(comma_pos) = after.find(',') {
                let err_str = after[comma_pos + 1..].trim_start_matches(|c: char| c.is_whitespace());
                let err_code = err_str.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
                if let Ok(code) = err_code.parse::<u32>() {
                    if code != 0 {
                        bail!("TCP connection failed: +CIPOPEN error code {}", code);
                    }
                }
            }
        }

        // Tell modem not to automatically response send data
        let recv_cmd = format!("AT+CIPRXGET=1");
        let _ = self.send_at_command(&recv_cmd, "OK", Duration::from_secs(10));

        info!("TCP connection established (socket 0)");
        Ok(0)
    }

    /// Close a TCP socket opened with `open_tcp_connection`.
    pub fn close_tcp_connection(&mut self, socket_id: u8) -> Result<()> {
        info!("Closing TCP connection (socket {})", socket_id);
        // AT+CIPCLOSE=<socket>
        let close_cmd = format!("AT+CIPCLOSE={}", socket_id);
        self.send_at_command(&close_cmd, "OK", Duration::from_secs(10))?;
        Ok(())
    }

    /// Send binary data over a TCP socket.
    ///
    /// SIMCom uses `AT+CIPSEND=<socket>,<len>` and waits for the `>`
    /// prompt just like Quectel's `AT+QISEND`.  The send-confirmation URC is
    /// `DATA ACCEPT` instead of `SEND OK`.
    pub fn send_tcp_data(&mut self, socket_id: u8, data: &[u8], retries: u8) -> Result<()> {
        // AT+CIPSEND=<socket>,<len> followed by the raw bytes.
        // IMPORTANT: the length we declare to the modem must exactly match the
        // number of bytes we subsequently write, otherwise the modem either
        // waits forever for more bytes or discards the excess — both resulting
        // in the server receiving garbage (e.g. the header block repeated to
        // fill the declared Content-Length).
        let send_cmd = format!("AT+CIPSEND={},{}", socket_id, data.len());

        self.send_at_command_silent(&send_cmd, ">", Duration::from_secs(5))?;

        // Write exactly data.len() bytes — no more, no less.
        self.uart.write(data)?;

        match self.wait_for_response("+CIPSEND:", Duration::from_secs(60), true) {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = self.send_at_command_silent("AT+CEER", "OK", Duration::from_secs(1));
                if retries > 0 {
                    info!("send_tcp_data error, retrying ({} left): {}", retries, e);
                    // Do NOT retry by recursing with the same data — the modem
                    // may have partially accepted it.  Instead cancel the current
                    // send session and re-open from the caller's level if needed.
                    // For now, surface the error and let the caller decide.
                }
                Err(e)
            }
        }
    }

    /// Receive TCP data from `socket_id`, reading until the modem reports
    /// `pending=0` (no more data buffered).
    ///
    /// `chunk_size` controls how many bytes are requested per `AT+CIPRXGET=2`
    /// call. `already_received` allows the caller to seed the buffer with bytes
    /// that arrived early (e.g. mixed in with a URC).
    pub fn receive_tcp_data(&mut self, socket_id: u8, chunk_size: usize, already_received: &[u8]) -> Vec<u8> {
        let mut all_data: Vec<u8> = Vec::new();
        all_data.extend_from_slice(already_received);

        let start   = std::time::Instant::now();
        let timeout = Duration::from_secs(120);
        let chunk   = chunk_size.min(4096);
        let mut buf = [0u8; 4096];

        loop {
            if start.elapsed() > timeout {
                info!("receive_tcp_data: timeout after {} bytes", all_data.len());
                break;
            }

            let recv_cmd = format!("AT+CIPRXGET=2,{},{}", socket_id, chunk);
            info!("Requesting {} bytes, {} received so far", chunk, all_data.len());
            self.uart.write(format!("{}
", recv_cmd).as_bytes()).ok();

            // Wait for the reply to *our* command specifically.
            // The response buffer may also contain "+CIPRXGET: 1,..." URCs —
            // searching for "+CIPRXGET: 2," skips them unambiguously.
            match self.wait_for_response("+CIPRXGET: 2,", Duration::from_secs(10), false) {
                Ok(response) => {
                    if let Some(header_start) = response.find("+CIPRXGET: 2,") {
                        let after = &response[header_start..];
                        if let Some(nl) = after.find("
") {
                            // Header line: "+CIPRXGET: 2,<socket>,<actual>,<pending>"
                            let fields: Vec<&str> = after[..nl].split(',').collect();
                            if fields.len() >= 4 {
                                let actual_len  = fields[2].trim().parse::<usize>().unwrap_or(0);
                                let pending_len = fields[3].trim().parse::<usize>().unwrap_or(0);

                                if actual_len == 0 {
                                    if pending_len == 0 {
                                        break; // all data consumed
                                    }
                                    thread::sleep(Duration::from_millis(50));
                                    continue;
                                }

                                // Bytes that arrived in the same UART read as the header.
                                let data_start = header_start + nl + 2;
                                let prefetched = if response.len() > data_start {
                                    let bytes = &response.as_bytes()[data_start..];
                                    all_data.extend_from_slice(bytes);
                                    bytes.len()
                                } else {
                                    0
                                };

                                // Drain the remainder directly from UART.
                                let mut need = actual_len.saturating_sub(prefetched);
                                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                                while need > 0 && std::time::Instant::now() < deadline {
                                    let to_read = need.min(buf.len());
                                    match self.uart.read(&mut buf[..to_read], 10) {
                                        Ok(n) if n > 0 => {
                                            all_data.extend_from_slice(&buf[..n]);
                                            need = need.saturating_sub(n);
                                        }
                                        _ => thread::sleep(Duration::from_millis(5)),
                                    }
                                }

                                if pending_len == 0 {
                                    break; // modem buffer empty, we are done
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // Timeout on this chunk — stop rather than spin.
                    break;
                }
            }
        }

        info!("receive_tcp_data: {} bytes total", all_data.len());
        all_data
    }

    // -----------------------------------------------------------------------
    // HTTP helpers (identical public API to QuectelModule)
    // -----------------------------------------------------------------------

    pub fn send_http_request(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse> {
        let url = url.strip_prefix("http://").unwrap_or(url);
        let (host, path) = if let Some(slash_pos) = url.find('/') {
            (&url[..slash_pos], &url[slash_pos..])
        } else {
            (url, "/")
        };

        let (host, port) = if let Some(colon_pos) = host.find(':') {
            let port = host[colon_pos + 1..].parse::<u16>().unwrap_or(80);
            (&host[..colon_pos], port)
        } else {
            (host, 80u16)
        };

        info!("Sending HTTP {} request to {}:{}{}", method, host, port, path);

        let socket_id = self.open_tcp_connection(host, port)?;

        // Build the entire HTTP request — headers and body — into a single
        // contiguous buffer up front.  This is the only reliable way to ensure
        // the modem sends the correct bytes: building header and body
        // separately and passing slices of each to send_tcp_data risks the
        // declared AT+CIPSEND length and the actual written bytes diverging,
        // which causes the modem to pad with whatever it has buffered (often
        // a repeat of the header bytes).
        let mut request: Vec<u8> = Vec::new();

        // Request line + mandatory headers.
        request.extend_from_slice(
            format!("{} {} HTTP/1.1\r\nHost: {}\r\n", method, path, host).as_bytes()
        );

        // Content-Length — use the caller's body length, not any intermediate
        // buffer length, so it is always accurate regardless of what we do
        // with the bytes below.
        if let Some(body) = body {
            if !headers
                .iter()
                .any(|&(key, _)| key.eq_ignore_ascii_case("content-length"))
            {
                request.extend_from_slice(
                    format!("Content-Length: {}\r\n", body.len()).as_bytes()
                );
            }
        }

        // Caller-supplied headers.
        for (key, value) in headers {
            request.extend_from_slice(format!("{}: {}\r\n", key, value).as_bytes());
        }

        // Blank line terminating the header section.
        request.extend_from_slice(b"Connection: close\r\n\r\n");

        // Body — appended directly so headers and body are contiguous.
        if let Some(body) = body {
            request.extend_from_slice(body);
        }

        let body_len = body.map(|b| b.len()).unwrap_or(0);
        info!("HTTP request: {} bytes ({} header + {} body)",
            request.len(), request.len() - body_len, body_len);

        // Send in 1460-byte chunks (A7670 AT+CIPSEND per-call limit).
        const MAX_SEGMENT: usize = 1460;
        let total = request.len();
        let mut total_sent = 0;
        for chunk in request.chunks(MAX_SEGMENT) {
            self.send_tcp_data(socket_id, chunk, 3)?;
            total_sent += chunk.len();
            if total > MAX_SEGMENT {
                info!("Sent {}/{} bytes ({:.1}%)", total_sent, total,
                    (total_sent as f32 / total as f32) * 100.0);
            }
        }

        thread::sleep(Duration::from_millis(500));

        // Wait for the data-ready URC. This also consumes any +IPCLOSE that
        // arrives alongside it, clearing the UART for the CIPRXGET=2 loop.
        let early_data = match self.wait_for_response("+CIPRXGET: 1,", Duration::from_secs(10), false) {
            Ok(urc) => {
                // Extract any body bytes that arrived in the same UART read as the URC.
                // Format: "...+CIPRXGET: 1,<socket>\r\n<possible early data>"
                if let Some(pos) = urc.find("+CIPRXGET: 1,") {
                    let after_urc = &urc[pos..];
                    if let Some(nl) = after_urc.find("\r\n") {
                        let tail = &after_urc[nl + 2..];
                        tail.as_bytes().to_vec()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }
            Err(_) => vec![],
        };

        let response = self.receive_tcp_data(socket_id, 4096, &early_data);

        let _ = self.close_tcp_connection(socket_id);

        let response_str = String::from_utf8_lossy(&response);
        info!("HTTP response received ({} bytes)", response.len());
        Ok(parse_http_response(&response_str))
    }

    pub fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse> {
        info!("Sending data via HTTP ({} bytes)...", body.len());

        let response = self
            .send_http_request("POST", url, headers, Some(body))
            .map_err(|e| anyhow::anyhow!("HTTP POST failed: {}", e))?;

        if response.status >= 200 && response.status < 400 {
            info!("HTTP POST successful");
        } else {
            bail!("HTTP POST failed with response: {:?}", response.body)
        }
        Ok(response)
    }

    pub fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        info!("Sending HTTP GET to {}", url);
        self.send_http_request("GET", url, headers, None)
    }

    // -----------------------------------------------------------------------
    // Misc
    // -----------------------------------------------------------------------

    pub fn is_connected(&self) -> bool {
        self.is_connected
    }
}


use crate::modem::{Modem, HttpResponse, parse_http_response};

impl<'a> Modem for SimcomModule<'a> {
    fn initialize_network(&mut self, apn: &str) -> Result<()> {
        self.initialize_network(apn)
    }
    fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.http_post(url, body, headers)
    }
    fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.http_get(url, headers)
    }
    fn battery_voltage(&mut self) -> Result<f32> {
        self.battery_voltage()
    }
    fn sleep(&mut self) -> Result<()> {
        self.sleep()
    }
    fn wake(&mut self) -> Result<()> {
        self.wake()
    }
    fn is_connected(&self) -> bool {
        self.is_connected()
    }
}
