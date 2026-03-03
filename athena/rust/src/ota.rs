use anyhow::{bail, Result};
use log::info;
use sha2::{Digest, Sha256};
use esp_idf_sys::{
    esp, esp_ota_begin, esp_ota_end, esp_ota_get_next_update_partition,
    esp_ota_handle_t, esp_ota_set_boot_partition, esp_ota_write,
    esp_restart, OTA_SIZE_UNKNOWN,
};

pub fn install_firmware(firmware: &[u8], expected_sha256: &str) -> Result<()> {
    if !verify_sha256(firmware, expected_sha256) {
        bail!("OTA: SHA-256 mismatch");
    }
    info!("OTA: SHA-256 matches");

    let partition = unsafe { esp_ota_get_next_update_partition(core::ptr::null()) };
    if partition.is_null() {
        bail!("OTA: no writable partition found");
    }

    let mut handle: esp_ota_handle_t = 0;
    unsafe {
        esp!(esp_ota_begin(partition, OTA_SIZE_UNKNOWN as usize, &mut handle))?;
        esp!(esp_ota_write(handle, firmware.as_ptr() as *const _, firmware.len()))?;
        esp!(esp_ota_end(handle))?;
        esp!(esp_ota_set_boot_partition(partition))?;
    }

    info!("OTA: rebooting...");
    unsafe { esp_restart() };
}

pub fn parse_response_header(raw_response: &str, header_name: &str) -> Option<String> {
    let header_section = raw_response.split("\r\n\r\n").next().unwrap_or(raw_response);
    for line in header_section.lines() {
        if let Some(rest) = line.strip_prefix(header_name) {
            if let Some(value) = rest.strip_prefix(':') {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn verify_sha256(data: &[u8], expected: &str) -> bool {
    let digest = Sha256::digest(data);
    if expected.len() != 64 {
        return false;
    }
    expected.bytes().zip(digest.iter().flat_map(|b| {
        let nibbles = [b >> 4, b & 0xf];
        nibbles.map(|n| if n < 10 { b'0' + n } else { b'a' + n - 10 })
    })).all(|(a, b)| a == b)
}
