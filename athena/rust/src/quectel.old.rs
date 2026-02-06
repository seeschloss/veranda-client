#![allow(dead_code)]

use esp_idf_hal::{
    uart::UartDriver,
    gpio::{OutputPin, PinDriver, Output},
};
use log::info;
use anyhow::{Result, bail};
use std::time::Duration;
use std::thread;

pub struct QuectelModule<'a, P: OutputPin> {
    uart: UartDriver<'a>,
    power_pin: PinDriver<'a, P, Output>,
}

impl<'a, P: OutputPin> QuectelModule<'a, P> {
    pub fn new(
        uart: UartDriver<'a>,
        power_pin: PinDriver<'a, P, Output>
    ) -> Self {
        Self { uart, power_pin }
    }

    pub fn power_on(&mut self) -> Result<()> {
        info!("Powering on EC200A module...");
        self.power_pin.set_high()?;
        thread::sleep(Duration::from_secs(3));
        Ok(())
    }

    pub fn test_communication(&mut self) -> Result<()> {
        info!("Testing EC200A communication...");
        self.send_at_command("AT", "OK", Duration::from_secs(5))?;
        info!("EC200A communication test successful");
        Ok(())
    }

    pub fn send_at_command(&mut self, command: &str, expected: &str, timeout: Duration) -> Result<()> {
        info!("Sending: {}", command);
        self.uart.write(format!("{}\r\n", command).as_bytes())?;
        self.wait_for_response(expected, timeout)?;
        Ok(())
    }

    pub fn wait_for_response(&mut self, expected: &str, timeout: Duration) -> Result<String> {
        let start = std::time::Instant::now();
        let mut response = String::new();
        let mut buffer = [0u8; 256];

        while start.elapsed() < timeout {
            match self.uart.read(&mut buffer, 100) {
                Ok(len) if len > 0 => {
                    let data = String::from_utf8_lossy(&buffer[..len]);
                    response.push_str(&data);
                    print!("{}", data); // Echo for debugging

                    if response.contains(expected) {
                        return Ok(response);
                    }

                    if response.contains("ERROR") {
                        bail!("AT command error: {}", response);
                    }
                }
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }

        bail!("Timeout waiting for: {}", expected);
    }

    pub fn disconnect(&mut self) -> Result<()> {
        info!("Disconnecting EC200A...");
        self.send_at_command("AT+QIDEACT=1", "OK", Duration::from_secs(10))?;
        Ok(())
    }

    pub fn initialize(&mut self, apn: &str) -> Result<()> {
        info!("Initializing EC200A module...");

        // Test communication
        self.send_at_command("AT", "OK", Duration::from_secs(5))?;

        // Disable echo
        self.send_at_command("ATE0", "OK", Duration::from_secs(1))?;

        // Check SIM card
        self.send_at_command("AT+CPIN?", "READY", Duration::from_secs(10))?;

        // Set APN
        let apn_cmd = format!("AT+QICSGP=1,1,\"{}\"", apn);
        self.send_at_command(&apn_cmd, "OK", Duration::from_secs(5))?;

        // Activate context
        self.send_at_command("AT+QIACT=1", "OK", Duration::from_secs(30))?;

        // Check IP address
        self.send_at_command("AT+QIACT?", "+QIACT:", Duration::from_secs(5))?;

        info!("Network connection established");
        Ok(())
    }

    pub fn send_photo_http(&mut self, image_data: &[u8], url: &str, headers: &[(&str, &str)]) -> Result<()> {
        info!("Sending photo via HTTP ({} bytes)...", image_data.len());

        // Configure HTTP context
        self.send_at_command("AT+QHTTPCFG=\"contextid\",1", "OK", Duration::from_secs(5))?;

        // Set request header for binary content
        self.send_at_command("AT+QHTTPCFG=\"requestheader\",1", "OK", Duration::from_secs(5))?;

        for (header, value) in headers {
            self.send_at_command(format!("AT+QHTTPCFG=\"reqheader/add\",\"{}\",\"{}\"", header, value).as_str(), "OK", Duration::from_secs(1))?;
        };

        // Set HTTP server URL
        let url_len = url.len();
        let url_cmd = format!("AT+QHTTPURL={},1", url_len);
        self.send_at_command(&url_cmd, "CONNECT", Duration::from_secs(5))?;

        // Send URL
        self.uart.write(url.as_bytes())?;
        self.wait_for_response("OK", Duration::from_secs(10))?;

        // Start HTTP POST
        let post_cmd = format!("AT+QHTTPPOST={},30,30", image_data.len());
        self.send_at_command(&post_cmd, "CONNECT", Duration::from_secs(10))?;

        // Send binary data in chunks
        const CHUNK_SIZE: usize = 1024;
        let mut total_sent = 0;

        for chunk in image_data.chunks(CHUNK_SIZE) {
            self.uart.write(chunk)?;
            total_sent += chunk.len();

            thread::sleep(Duration::from_millis(10));

            if total_sent % (CHUNK_SIZE * 10) == 0 {
                info!("Sent: {}/{} bytes ({:.1}%)", 
                     total_sent, 
                     image_data.len(), 
                     (total_sent as f32 / image_data.len() as f32) * 100.0);
            }
        }

        info!("Binary upload complete: {} bytes sent", total_sent);

        // Wait for HTTP response
        let response = self.wait_for_response("+QHTTPPOST:", Duration::from_secs(60))?;
        if response.contains("200") {
            info!("HTTP POST successful");
            Ok(())
        } else {
            bail!("HTTP POST failed: {}", response);
        }
    }
}
