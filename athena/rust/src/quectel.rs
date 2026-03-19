#![allow(dead_code)]

use esp_idf_hal::{
    uart::UartDriver,
    units::Hertz,
    gpio::{PinDriver, Output},
};
use log::info;
use anyhow::{anyhow, Result, bail};
use std::time::Duration;
use std::thread;

#[derive(Debug)]
pub struct QuectelError {
    details: String
}

impl QuectelError {
    pub fn new(msg: &str)-> Self {
        Self{details: msg.to_owned()}
    }
}
impl std::fmt::Display for QuectelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>)-> Result<(), std::fmt::Error> {
        write!(f, "{}", self.details)
    }
}
impl std::error::Error for QuectelError {
    fn description(&self) -> &str {
        &self.details
    }
}

pub struct QuectelModule<'a> {
    uart: UartDriver<'a>,
    power_pin: PinDriver<'a, Output>,
    sleep_pin: Option<PinDriver<'a, Output>>,
    is_connected: bool,
}

impl<'a> QuectelModule<'a> {
    pub fn new(
        uart: UartDriver<'a>,
        power_pin: PinDriver<'a, Output>,
        sleep_pin: Option<PinDriver<'a, Output>>
    ) -> Self {
        Self {
            uart,
            power_pin,
            sleep_pin,
            is_connected: false,
        }
    }

    pub fn power_on(&mut self) -> Result<()> {
        info!("Powering on modem...");
        self.power_pin.set_high()?;
        thread::sleep(Duration::from_secs(3));
        self.send_at_command("AT+CFUN=1", "OK", Duration::from_secs(10))?;
        thread::sleep(Duration::from_secs(3));
        Ok(())
    }

    pub fn power_off(&mut self) -> Result<()> {
        info!("Powering off modem...");
        self.send_at_command("AT+CFUN=0", "OK", Duration::from_secs(10))?;
        thread::sleep(Duration::from_secs(1));
        self.power_pin.set_low()?;
        self.is_connected = false;
        Ok(())
    }

    pub fn sleep(&mut self) -> Result<()> {
        if let Some(sleep_pin) = self.sleep_pin.as_mut() {
            info!("Putting modem to sleep...");
            //sleep_pin.set_high()?;
            sleep_pin.set_low()?;
            self.send_at_command("AT&D0", "OK", Duration::from_secs(10))?;
            self.send_at_command("AT+QSCLK=1", "OK", Duration::from_secs(10))?;
            thread::sleep(Duration::from_millis(300));
            Ok(())
        } else {
            Err(anyhow!("Cannot put modem to sleep: no DTR connection"))
        }
    }

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

    pub fn send_at_command_silent(&mut self, command: &str, expected: &str, timeout: Duration) -> Result<String> {
        self.uart.write(format!("{}\r\n", command).as_bytes())?;
        self.wait_for_response(expected, timeout, true)
    }

    pub fn send_at_command(&mut self, command: &str, expected: &str, timeout: Duration) -> Result<String> {
        info!("Sending: {}", command);
        self.uart.write(format!("{}\r\n", command).as_bytes())?;
        self.wait_for_response(expected, timeout, false)
    }

    pub fn send_at_command_until(&mut self, command: &str, expected: &str, timeout: Duration, tries: i32) -> Result<String, QuectelError> {
        let mut retries = tries;

        let mut result = Err(QuectelError::new("plop"));

        while retries > 0 {
            match self.send_at_command(command, expected, timeout) {
                Ok(response) => {
                    if response.contains(expected) {
                        return Ok(response);
                    } else {
                        result = Err(QuectelError::new(format!("Expected response containing `{}` but received `{}`", expected, response).as_str()));
                    }
                },
                Err(e) => {
                    info!("SIM card not ready ({})", e);
                    result = Err(QuectelError::new(e.to_string().as_str()));
                },
            };

            retries -= 1;
            if retries > 0 {
                thread::sleep(Duration::from_secs(1));
            }
        }

        result
    }

    pub fn wait_for_response(&mut self, expected: &str, timeout: Duration, silent: bool) -> Result<String> {
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

    pub fn detect_and_set_uart_speed(&mut self, target_speed: Hertz) -> Result<()> {
        let common_rates = vec![115200, 230400, 460800, 921600];
        let target_rate: u32 = target_speed.into();

        info!("Detecting current UART speed and changing to {} baud", target_rate);

        let boot_deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut current_detected_rate = None;

        'outer: while std::time::Instant::now() < boot_deadline {
            for &rate in &common_rates {
                if self.uart.change_baudrate(Hertz(rate)).is_err() { continue; }
                thread::sleep(Duration::from_millis(100));
                if self.send_at_command("AT", "OK", Duration::from_millis(500)).is_ok() {
                    info!("Found current baudrate: {} baud", rate);
                    current_detected_rate = Some(rate);
                    break 'outer;
                }
            }
            info!("Modem not responding yet, waiting...");
            thread::sleep(Duration::from_millis(500));
        }

        match current_detected_rate {
            Some(current_rate) if current_rate == target_rate => {
                info!("Already at target baudrate {} baud", target_rate);
                Ok(())
            },
            Some(_) => {
                self.set_uart_speed(target_speed)
            },
            None => {
                bail!("Could not detect current UART baudrate");
            }
        }
    }

    pub fn set_uart_speed(&mut self, speed: Hertz) -> Result<()> {
        let supported_rates = vec![4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000];
        let rate_u32: u32 = speed.into();

        // Check if the requested rate is supported
        if !supported_rates.contains(&rate_u32) {
            bail!("Unsupported baud rate: {}", rate_u32);
        }

        // Check current baudrate to avoid unnecessary changes
        let current_rate: u32 = self.uart.baudrate()?.into();
        if rate_u32 == current_rate {
            info!("UART already at {} baud", rate_u32);
            return Ok(());
        }

        info!("Changing UART speed from {} to {} baud", current_rate, rate_u32);

        // Send AT+IPR command using the existing method
        let cmd = format!("AT+IPR={}", rate_u32);
        self.send_at_command(&cmd, "OK", Duration::from_secs(5))?;

        // Give the modem time to change its baudrate
        thread::sleep(Duration::from_millis(500));

        // Now change ESP32's baudrate
        self.uart.change_baudrate(speed)?;

        // Give some settling time
        thread::sleep(Duration::from_millis(100));

        // Test communication at new baudrate
        match self.test_communication() {
            Ok(()) => {
                info!("UART speed successfully changed to {} baud", rate_u32);
                Ok(())
            },
            Err(e) => {
                info!("Failed to communicate at new rate, attempting recovery...");
                // Try to recover by going back to original baudrate
                let original_rate = Hertz(current_rate);
                if let Err(_) = self.uart.change_baudrate(original_rate) {
                    bail!("Failed to recover original UART speed");
                }
                bail!("UART speed change failed: {}", e);
            }
        }
    }

    fn test_communication(&mut self) -> Result<()> {
        // Try a few times in case there are timing issues
        for attempt in 1..=3 {
            match self.send_at_command("AT", "OK", Duration::from_millis(1000)) {
                Ok(_) => return Ok(()),
                Err(_) if attempt < 3 => {
                    info!("Communication test attempt {} failed, retrying...", attempt);
                    thread::sleep(Duration::from_millis(100));
                },
                Err(e) => return Err(e),
            }
        }
        bail!("Communication test failed after 3 attempts");
    }

    pub fn battery_voltage(&mut self) -> Result<f32> {
        let response = self.send_at_command("AT+CBC", "OK", Duration::from_secs(30))
            .map_err(|e| anyhow!("AT+CBC command failure: {}", e))?;

        // Find "+CBC:" as a substring rather than relying on token position.
        // The BG95/EC200A format is: +CBC: <bcs>,<bcl>,<mV>
        // Using nth(1) on split_whitespace() was fragile: any leading garbage
        // byte shifted all tokens and caused a silent parse failure.
        if let Some(pos) = response.find("+CBC:") {
            let after = response[pos + 5..].trim_start();
            if let Some(mv_str) = after.split(',').nth(2) {
                let mv_clean: String = mv_str.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if !mv_clean.is_empty() {
                    return mv_clean.parse::<f32>()
                        .map(|v| v / 1000.0)
                        .map_err(|e| anyhow!("Failed to parse CBC voltage '{}': {}", mv_clean, e));
                }
            }
        }

        Err(anyhow!("No +CBC: field in response: {}", response))
    }

    pub fn initialize_network(&mut self, apn: &str) -> Result<()> {
        info!("Initializing network connection...");

        //self.detect_and_set_uart_speed(Hertz(921600))?;
        //self.detect_and_set_uart_speed(Hertz(460800))?;
        self.detect_and_set_uart_speed(Hertz(230400))?;

        // Test communication
        self.send_at_command_until("AT", "OK", Duration::from_secs(5), 30)?;

        // Disable echo
        self.send_at_command("ATE0", "OK", Duration::from_secs(1))?;

        // Check SIM card
        self.send_at_command_until("AT+CPIN?", "OK", Duration::from_secs(1), 10)?;

        // Verbose debugging
        //self.send_at_command("AT+CMEE=2", "OK", Duration::from_secs(30))?;

        // Signal quality
        self.send_at_command("AT+CSQ", "OK", Duration::from_secs(30))?;

        //info!("10.1");
        //thread::sleep(Duration::from_millis(10_000));
        //info!("10.2");
        //FreeRtos::delay_ms(30_000);
        //info!("10.3");

        // Set COPS format to "long alphanumeric"
        //self.send_at_command("AT+COPS=3,0", "OK", Duration::from_secs(120))?;

        // Available operators
        //self.send_at_command("AT+COPS=?", "OK", Duration::from_secs(120))?;
        //self.send_at_command("AT+COPS?", "OK", Duration::from_secs(30))?;
        //self.send_at_command("AT+COPS=0", "OK", Duration::from_secs(30))?;

        // Base
        //self.send_at_command("AT+COPS=4,\"20620\"", "OK", Duration::from_secs(30))?;

        // Orange
        //self.send_at_command("AT+COPS=4,\"20610\"", "OK", Duration::from_secs(30))?;

        // Proximus
        //self.send_at_command("AT+COPS=4,2,\"20601\"", "OK", Duration::from_secs(120))?;

        // Digi
        //self.send_at_command("AT+COPS=4,\"20612\"", "OK", Duration::from_secs(30))?;

        // Configure PDP context
        let pdp_cmd = format!("AT+CGDCONT=1,\"IP\",\"{}\"", apn);
        self.send_at_command(&pdp_cmd, "OK", Duration::from_secs(5))?;

        self.send_at_command("AT+CGACT=0,1", "OK", Duration::from_secs(30))?;

        thread::sleep(Duration::from_millis(1000));
        //FreeRtos::delay_ms(10_000);

        // Get PDP context
        self.send_at_command("AT+CGDCONT?", "OK", Duration::from_secs(30))?;

        // Register
        self.send_at_command("AT+CREG=1", "OK", Duration::from_secs(30))?;

        // Get registration status
        self.send_at_command("AT+CREG?", "OK", Duration::from_secs(30))?;
        self.send_at_command("AT+CGREG?", "OK", Duration::from_secs(30))?;
        self.send_at_command("AT+CEREG?", "OK", Duration::from_secs(30))?;

        // Add logging
        self.send_at_command("AT+CGREG=1", "OK", Duration::from_secs(30))?;
        self.send_at_command("AT+CEREG=1", "OK", Duration::from_secs(30))?;

        // Activate PDP context
        self.send_at_command("AT+CGACT=1,1", "OK", Duration::from_secs(30))?;
        // Also activate the Quectel socket stack context
        match self.send_at_command("AT+QIACT=1", "OK", Duration::from_secs(30)) {
            Ok(_) => {},
            Err(e) => {
                // May already be active — check rather than bail
                let status = self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(10))?;
                if !status.contains("+QIACT:") {
                    return Err(e);
                }
                info!("QIACT already active, continuing...");
            }
        }

        // Check if we got an IP address
        self.send_at_command("AT+CGPADDR=1", "+CGPADDR:", Duration::from_secs(5))?;

        self.is_connected = true;
        info!("Network connection established");
        Ok(())
    }

    pub fn open_tcp_connection(&mut self, host: &str, port: u16) -> Result<u8> {
        if !self.is_connected {
            bail!("Network not connected. Call initialize_network first.");
        }

        // Ensure the Quectel socket stack's PDP context is active.
        // AT+CGACT activates the 3GPP bearer; AT+QIACT activates the separate
        // Quectel socket-layer context that AT+QIOPEN requires.
        // If QIACT? returns no "+QIACT:" line, activate it now.
        let qiact_status = self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(10))
            .unwrap_or_default();
        if !qiact_status.contains("+QIACT:") {
            info!("QIACT context not active, activating...");
            self.send_at_command("AT+QIACT=1", "OK", Duration::from_secs(30))
                .map_err(|e| anyhow!("Failed to activate QIACT context: {}", e))?;
            // Confirm it's up
            let check = self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(10))?;
            if !check.contains("+QIACT:") {
                bail!("QIACT context activation did not produce an IP address");
            }
        }

        // Close socket 0 only if it's actually open
        let state = self.send_at_command("AT+QISTATE=0,0", "OK", Duration::from_secs(5))
            .unwrap_or_default();
        if state.contains("+QISTATE:") {
            let _ = self.send_at_command("AT+QICLOSE=0", "OK", Duration::from_secs(10));
            thread::sleep(Duration::from_millis(500));
        }

        info!("Opening TCP connection to {}:{}", host, port);
        let connect_cmd = format!("AT+QIOPEN=1,0,\"TCP\",\"{}\",{},0,0", host, port);
        self.send_at_command(&connect_cmd, "OK", Duration::from_secs(10))?;

        // Wait for async URC and check its error code
        let urc = self.wait_for_response("+QIOPEN:", Duration::from_secs(150), false)?;

        // Parse "+QIOPEN: <socket>,<err>" — err must be 0
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
                           (566=QIACT not ready, check AT+QIACT=1)", code);
                    }
                }
            }
        }

        info!("TCP connection established (socket 0)");
        Ok(0)
    }

    pub fn close_tcp_connection(&mut self, socket_id: u8) -> Result<()> {
        info!("Closing TCP connection (socket {})", socket_id);
        let close_cmd = format!("AT+QICLOSE={}", socket_id);
        self.send_at_command(&close_cmd, "OK", Duration::from_secs(10))?;
        Ok(())
    }

    pub fn send_tcp_data(&mut self, socket_id: u8, data: &[u8], retries: u8) -> Result<()> {
        let send_cmd = format!("AT+QISEND={},{}", socket_id, data.len());
        self.send_at_command_silent(&send_cmd, ">", Duration::from_secs(5))?;

        // Send the actual binary data
        self.uart.write(data)?;

        // Wait for send confirmation
        if let Err(e) = self.wait_for_response("SEND OK", Duration::from_secs(10), true) {
            self.send_at_command_silent("AT+QIGETERROR", "OK", Duration::from_secs(1))?;
            if retries > 0 {
                info!("Got an error, retrying after a pause");
                thread::sleep(Duration::from_millis(1000));
                return self.send_tcp_data(socket_id, data, retries - 1)
            } else {
                return Err(e)
            }
        }
        Ok(())
    }

    /// Read exactly one AT+QIRD chunk from the modem.
    /// Returns the raw payload bytes for that chunk (not including AT framing).
    /// Returns an empty Vec if the modem reports 0 bytes available.
    fn read_one_qird_chunk(&mut self, socket_id: u8, max_len: usize) -> Result<Vec<u8>> {
        self.uart.write(format!("AT+QIRD={},{}\r\n", socket_id, max_len).as_bytes())?;

        let timeout = Duration::from_secs(10);
        let start = std::time::Instant::now();
        let mut at_buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 256];

        // Collect bytes until we see the "+QIRD: <len>\r\n" header line
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
                    let len_str = after.trim();
                    declared_len = len_str.parse::<usize>()
                        .map_err(|_| anyhow::anyhow!("Bad +QIRD: length: {}", len_str))?;
                    at_buf.drain(..pos + 2);
                    break;
                }
                // Discard other lines (echo, blank lines)
                at_buf.drain(..pos + 2);
            }
        }

        if declared_len == 0 {
            return Ok(Vec::new());
        }

        info!("QIRD chunk: declared {} bytes", declared_len);

        // at_buf already holds whatever arrived alongside the header line.
        let mut payload: Vec<u8> = at_buf;
        let start2 = std::time::Instant::now();
        while payload.len() < declared_len {
            if start2.elapsed() > timeout {
                bail!("Timeout reading QIRD payload ({}/{} bytes)", payload.len(), declared_len);
            }
            match self.uart.read(&mut tmp, 100) {
                Ok(n) if n > 0 => payload.extend_from_slice(&tmp[..n]),
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }

        // payload may contain bytes beyond declared_len (the trailing \r\nOK\r\n
        // and possibly the start of the next +QIURC or +QIRD header).
        // We MUST NOT call wait_for_response("OK") here — that would consume
        // bytes that belong to the next chunk.  Instead, just discard everything
        // after declared_len; the AT framing bytes (\r\nOK\r\n) are only 6 bytes
        // and will be skipped by the header-scanning loop on the next call because
        // it discards any line that doesn't start with "+QIRD:".
        payload.truncate(declared_len);

        Ok(payload)
    }

    pub fn receive_tcp_data(&mut self, socket_id: u8, max_len: usize) -> Vec<u8> {
        let recv_cmd = format!("AT+QIRD={},{}", socket_id, max_len);
        // First, wait just for the +QIRD: header line to learn the data length
        if let Ok(header_response) = self.send_at_command(&recv_cmd, "+QIRD:", Duration::from_secs(10)) {

            // Parse the declared data length from the +QIRD: header
            // Format: +QIRD: <data_len>\r\n<data>\r\nOK
            if let Some(data_start) = header_response.find("+QIRD:") {
                let after_qird = &header_response[data_start + 6..];
                if let Some(newline_pos) = after_qird.find("\r\n") {
                    let data_len_str = after_qird[..newline_pos].trim();
                    info!("QIRD length: {}", data_len_str);
                    if let Ok(data_len) = data_len_str.parse::<usize>() {
                        if data_len == 0 {
                            return Vec::new();
                        }

                        // We already have some bytes after the header in header_response.
                        // Keep reading until we have all data_len bytes (plus trailing OK).
                        let data_offset = data_start + 6 + newline_pos + 2;
                        let already_have = header_response.as_bytes()
                            .get(data_offset..)
                            .map(|s| s.to_vec())
                            .unwrap_or_default();

                        let mut data_buf: Vec<u8> = already_have;

                        // Read more until we have all data_len bytes
                        if data_buf.len() < data_len {
                            let remaining = data_len - data_buf.len();
                            // wait_for_response with "OK" will keep reading until the trailing OK
                            // but we really just need enough bytes; use a generous timeout
                            let start = std::time::Instant::now();
                            let timeout = Duration::from_secs(10);
                            let mut tmp = [0u8; 256];
                            while data_buf.len() < data_len && start.elapsed() < timeout {
                                match self.uart.read(&mut tmp, 100) {
                                    Ok(n) if n > 0 => data_buf.extend_from_slice(&tmp[..n]),
                                    _ => thread::sleep(Duration::from_millis(10)),
                                }
                            }
                            let _ = remaining; // suppress unused warning
                        }

                        info!("QIRD length (parsed): {} (received: {})", data_len, data_buf.len());

                        // Drain any trailing \r\nOK\r\n from the modem
                        let _ = self.wait_for_response("OK", Duration::from_secs(3), true);

                        return data_buf[..data_len.min(data_buf.len())].to_vec();
                    }
                }
            }
        }

        Vec::new()
    }

    pub fn send_http_request(&mut self, method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<HttpResponse> {
        // Parse URL to extract host and path
        let url = url.strip_prefix("http://").unwrap_or(url);
        let (host, path) = if let Some(slash_pos) = url.find('/') {
            (&url[..slash_pos], &url[slash_pos..])
        } else {
            (url, "/")
        };

        // Extract port if specified
        let (host, port) = if let Some(colon_pos) = host.find(':') {
            let port_str = &host[colon_pos + 1..];
            let port = port_str.parse::<u16>().unwrap_or(80);
            (&host[..colon_pos], port)
        } else {
            (host, 80)
        };

        info!("Sending HTTP {} request to {}:{}{}", method, host, port, path);

        // Open TCP connection
        let socket_id = self.open_tcp_connection(host, port)?;

        // Build HTTP request
        let mut request = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", method, path, host);

        // Add caller-supplied headers
        for (key, value) in headers {
            request.push_str(&format!("{}: {}\r\n", key, value));
        }

        // Add Content-Length if we have a body and the caller hasn't provided one
        if let Some(body) = body {
            if !headers.iter().any(|&(key, _)| key.eq_ignore_ascii_case("content-length")) {
                request.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
        }

        request.push_str("Connection: close\r\n");
        request.push_str("\r\n");

        // Send HTTP request headers
        self.send_tcp_data(socket_id, request.as_bytes(), 3)?;

        // Send body if present
        if let Some(body) = body {
            const CHUNK_SIZE: usize = 1024;
            let mut total_sent = 0;
            for chunk in body.chunks(CHUNK_SIZE) {
                self.send_tcp_data(socket_id, chunk, 10)?;
                total_sent += chunk.len();
                thread::sleep(Duration::from_millis(10));
                if total_sent % (CHUNK_SIZE * 10) == 0 {
                    info!("Sent: {}/{} bytes ({:.1}%)",
                         total_sent, body.len(),
                         (total_sent as f32 / body.len() as f32) * 100.0);
                }
            }
        }

        // No need to wait for +QIURC: "recv" — in manual-receive mode (access
        // mode 0) we can poll AT+QIRD directly.  The modem returns +QIRD: 0
        // when nothing is buffered yet; we just back off briefly and retry.
        // URCs (+QIURC: "recv", +QIURC: "closed") will appear in the UART
        // stream but read_one_qird_chunk discards any line that isn't +QIRD:.

        // ---------------------------------------------------------------
        // Receive the full HTTP response as raw bytes, polling AT+QIRD.
        // We work in bytes throughout so binary bodies (firmware images)
        // are never mangled by a UTF-8 decoder.
        // ---------------------------------------------------------------
        const QIRD_CHUNK: usize = 1460;
        let mut raw: Vec<u8> = Vec::new();
        let mut content_length: Option<usize> = None;
        let mut header_end: Option<usize> = None;
        let mut consecutive_empty: u32 = 0;
        const MAX_EMPTY: u32 = 100; // 100 × 50 ms = 5 s stall timeout

        loop {
            let chunk = self.read_one_qird_chunk(socket_id, QIRD_CHUNK)?;
            if chunk.is_empty() {
                // Nothing buffered yet — check if we already have everything
                if let (Some(hdr_end), Some(cl)) = (header_end, content_length) {
                    if raw.len().saturating_sub(hdr_end) >= cl {
                        break;
                    }
                }
                consecutive_empty += 1;
                if consecutive_empty >= MAX_EMPTY {
                    bail!("Stalled: no data received for 5 s ({}/{} bytes)",
                          raw.len().saturating_sub(header_end.unwrap_or(0)),
                          content_length.unwrap_or(0));
                }
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            consecutive_empty = 0;
            raw.extend_from_slice(&chunk);

            // Find header/body separator and Content-Length once
            if header_end.is_none() {
                if let Some(sep) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(sep + 4);
                    let header_str = String::from_utf8_lossy(&raw[..sep]);
                    for line in header_str.lines() {
                        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            if let Ok(n) = rest.trim().parse::<usize>() {
                                content_length = Some(n);
                                info!("HTTP Content-Length: {}", n);
                            }
                        }
                    }
                }
            }

            // Stop as soon as we have all body bytes
            if let (Some(hdr_end), Some(cl)) = (header_end, content_length) {
                let body_received = raw.len().saturating_sub(hdr_end);
                if body_received % (64 * 1024) < QIRD_CHUNK {
                    info!("HTTP body progress: {}/{} bytes", body_received, cl);
                }
                if body_received >= cl {
                    break;
                }
            }
        }

        let _ = self.close_tcp_connection(socket_id);
        info!("HTTP response received ({} total bytes)", raw.len());

        Ok(parse_http_response_bytes(&raw))
    }

    pub fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse> {
        info!("Sending data via HTTP ({} bytes)...", body.len());

        let response = self
            .send_http_request("POST", url, headers, Some(body))
            .map_err(|e| anyhow::anyhow!("HTTP POST failed: {}", e))?;

        if response.status >= 200 && response.status < 400 {
            info!("HTTP POST successful");
        } else {
            bail!("HTTP POST failed with response: {:?}", response.body);
        }
        Ok(response)
    }

    pub fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        info!("Sending HTTP GET to {}", url);
        self.send_http_request("GET", url, headers, None)
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected
    }
}

use crate::modem::{Modem, HttpResponse, parse_http_response_bytes};

impl<'a> Modem for QuectelModule<'a> {
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
