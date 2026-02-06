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
use esp_idf_hal::delay::FreeRtos;

pub struct QuectelModule<'a, P1: OutputPin, P2: OutputPin> {
    uart: UartDriver<'a>,
    power_pin: PinDriver<'a, P1, Output>,
    sleep_pin: Option<PinDriver<'a, P2, Output>>,
    is_connected: bool,
}

impl<'a, P1: OutputPin, P2: OutputPin> QuectelModule<'a, P1, P2> {
    pub fn new(
        uart: UartDriver<'a>,
        power_pin: PinDriver<'a, P1, Output>,
        sleep_pin: Option<PinDriver<'a, P2, Output>>
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

        // Try to detect current baudrate by testing communication
        let mut current_detected_rate = None;

        for &rate in &common_rates {
            info!("Testing communication at {} baud", rate);

            if let Ok(_) = self.uart.change_baudrate(Hertz(rate)) {
                thread::sleep(Duration::from_millis(100));

                // Test communication with a short timeout using send_at_command
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
            },
            Some(_) => {
                // Now change to target speed
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
        match self.send_at_command("AT+CBC", "OK", Duration::from_secs(30)) {
            Ok(str_raw_response) => match str_raw_response.split_whitespace().nth(1) {
                Some(str_response_data) => match str_response_data.split(",").nth(2) {
                    Some(str_voltage_field) => match str_voltage_field.parse::<f32>() {
                        Ok(voltage_float) => Ok(voltage_float / 1000.0),
                        Err(e) => Err(anyhow!("Failed to parse CBC voltage: {}", e)),
                    },
                    None => Err(anyhow!("No voltage field in response"))
                },
                None => Err(anyhow!("Couldn't find response field"))
            },
            Err(e) => Err(anyhow!("AT+CBC command failure: {}", e)),
        }
    }

    pub fn initialize_network(&mut self, apn: &str) -> Result<()> {
        info!("Initializing network connection...");

        //self.detect_and_set_uart_speed(Hertz(921600))?;
        //self.detect_and_set_uart_speed(Hertz(460800))?;

        // Test communication
        self.send_at_command("AT", "OK", Duration::from_secs(5))?;

        // Disable echo
        self.send_at_command("ATE0", "OK", Duration::from_secs(1))?;

        // Check SIM card
        let mut retries = 10;
        while retries > 0 {
            match self.send_at_command("AT+CPIN?", "READY", Duration::from_secs(1)) {
                Ok(response) => {
                    if response.contains("READY") {
                        retries = 0;
                        info!("SIM card ready ({})", response);
                    } else {
                        info!("SIM card not ready ({})", response);
                    }
                },
                Err(e) => {
                    info!("SIM card not ready ({})", e);
                },
            }

            retries -= 1;
        }

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

        self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(30))?;

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

        let _ = self.send_at_command("AT+QISTATE", "+QISTATE:", Duration::from_secs(5));
        //let _ = self.send_at_command("AT+QICSGP=1,1,\"simbase\",\"\",\"\",0", "OK", Duration::from_secs(5));
        let _ = self.send_at_command("AT+QICSGP=1", "OK", Duration::from_secs(5));
        //let _ = self.send_at_command("AT+QIACT=1", "OK", Duration::from_secs(5));
        let _ = self.send_at_command("AT+QIACT?", "OK", Duration::from_secs(5));

        let _ = self.send_at_command("AT+QIDNSCFG=1", "OK", Duration::from_secs(5));
//        let _ = self.send_at_command("AT+QIDNSGIP=1,\"www.google.com\"", "OK", Duration::from_secs(5));
//        self.wait_for_response("+QIURC", Duration::from_secs(15))?;

        //info!("10.1");
        //thread::sleep(Duration::from_millis(10_000));
        //info!("10.2");
        //FreeRtos::delay_ms(10_000);
        //info!("10.3");

        //let _ = self.send_at_command("AT+QPING=1,\"8.8.8.8\"", "OK", Duration::from_secs(5));
        //self.wait_for_response("QPING", Duration::from_secs(150))?;

        let _ = self.send_at_command("AT+QICLOSE=0", "OK", Duration::from_secs(5));
        //let _ = self.send_at_command("AT+QPING=1,8.8.8.8", "OK", Duration::from_secs(5));
        //let _ = self.send_at_command("AT+QPING=1,\"8.8.8.8\"", "OK", Duration::from_secs(5));
        //let _ = self.send_at_command("AT+QISTATE?", "OK", Duration::from_secs(5));

        //let dns_cmd = format!("AT+QIDNSGIP=1,\"{}\"", host);
        //self.send_at_command(&dns_cmd, "OK", Duration::from_secs(150))?;
        //self.wait_for_response("+QIURC", Duration::from_secs(150))?;

        info!("Opening TCP connection to {}:{}", host, port);

        // Open TCP socket connection
        let connect_cmd = format!("AT+QIOPEN=1,0,\"TCP\",\"{}\",{},0,0", host, port);
        self.send_at_command(&connect_cmd, "OK", Duration::from_secs(10))?;

        // Wait for connection confirmation
        self.wait_for_response("+QIOPEN", Duration::from_secs(150), false)?;

        info!("TCP connection established (socket 0)");
        Ok(0) // Return socket ID
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

    pub fn receive_tcp_data(&mut self, socket_id: u8, max_len: usize) -> Vec<u8> {
        let recv_cmd = format!("AT+QIRD={},{}", socket_id, max_len);
        if let Ok(response) = self.send_at_command(&recv_cmd, "+QIRD:", Duration::from_secs(10)) {

            info!("QIRD response: {}", response);

            // Parse the response to extract the actual data
            // Format: +QIRD: <data_len>\r\n<data>
            if let Some(data_start) = response.find("+QIRD:") {
                let after_qird = &response[data_start + 6..];
                if let Some(newline_pos) = after_qird.find("\r\n") {
                    let data_len_str = after_qird[..newline_pos].trim();
                    info!("QIRD length: {}", data_len_str);
                    if let Ok(data_len) = data_len_str.parse::<usize>() {
                        info!("QIRD length (parsed): {} (response length: {})", data_len, response.len());
                        if data_len > 0 {
                            let data_start_idx = data_start + 6 + newline_pos + 2;
                            if response.len() >= data_start_idx + data_len {
                                return response.as_bytes()[data_start_idx..data_start_idx + data_len].to_vec();
                            }
                        }
                    }
                }
            }
        }

        "".as_bytes().to_vec()
    }

    pub fn send_http_request(&mut self, method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<String> {
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

        // If there's a body and thhere is no explicit "Content-Length" header, let's add one
        if let Some(body) = body {
            if !headers.iter().any(|&(key, _)| key.eq_ignore_ascii_case("content-length")) {
                request.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
        }

        // Add headers
        for (key, value) in headers {
            request.push_str(&format!("{}: {}\r\n", key, value));
        }

        // Add content length if we have a body
        if let Some(body) = body {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }

        // Add connection close header
        request.push_str("Connection: close\r\n");
        request.push_str("\r\n");

        // Send HTTP request headers
        self.send_tcp_data(socket_id, request.as_bytes(), 3)?;

        // Send body if present
        if let Some(body) = body {
            // Cannot exceed 1460 bytes
            //const CHUNK_SIZE: usize = 512;
            const CHUNK_SIZE: usize = 1024;
            let mut total_sent = 0;

            for chunk in body.chunks(CHUNK_SIZE) {
                self.send_tcp_data(socket_id, chunk, 10)?;
                total_sent += chunk.len();

                thread::sleep(Duration::from_millis(10)); // Small delay between chunks

                if total_sent % (CHUNK_SIZE * 100) == 0 {
                    info!("Sent: {}/{} bytes ({:.1}%)", 
                         total_sent, 
                         body.len(), 
                         (total_sent as f32 / body.len() as f32) * 100.0);
                }
            }
        }

        // Read response
        //self.wait_for_response("+QIURC", Duration::from_secs(10))?;

        self.wait_for_response("+QIURC: \"recv\"", Duration::from_secs(5), false)?;

        let response = self.receive_tcp_data(socket_id, 1024);

        // Close connection
        let _ = self.close_tcp_connection(socket_id);

        let response_str = String::from_utf8_lossy(&response);
        info!("HTTP response received ({} bytes)", response.len());

        Ok(response_str.to_string())
    }

    pub fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<()> {
        info!("Sending data via HTTP ({} bytes)...", body.len());

        let response = match self.send_http_request("POST", url, headers, Some(body)) {
            Ok(response) => {
                response
            },
            Err(err) => {
                bail!("HTTP POST failed with error: {}", err);
            },
        };

        // Check if response indicates success
        if response.contains("HTTP/1.1 200") || response.contains("HTTP/1.0 200") {
            info!("HTTP POST successful");
            Ok(())
        } else {
            bail!("HTTP POST failed with response: {}", response);
        }
    }

    pub fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<String> {
        info!("Sending HTTP GET to {}", url);
        self.send_http_request("GET", url, headers, None)
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected
    }
}
