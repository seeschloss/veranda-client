use esp_idf_hal::{
    gpio::PinDriver,
    i2c::{self, I2cDriver},
    peripherals::Peripherals,
    uart::{UartConfig, UartDriver},
    units::Hertz,
};
use esp_idf_sys::{self as _, *};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use log::*;
use std::time::Duration;
use std::thread;

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ina3221::INA3221;

mod board;
mod camera;
mod modem;
mod ota;
mod power;

#[cfg(feature = "modem-wifi")]
mod wifi;
#[cfg(feature = "modem-wifi")]
use wifi::WifiModem;

#[cfg(feature = "modem-simcom")]
mod simcom;
#[cfg(feature = "modem-simcom")]
use simcom::SimcomModule as Modem;

#[cfg(feature = "modem-quectel")]
mod quectel;
#[cfg(feature = "modem-quectel")]
use quectel::QuectelModule as Modem;

// ---------------------------------------------------------------------------
// Firmware identity tags (searchable in the binary)
// ---------------------------------------------------------------------------

const FIRMWARE_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR")
);

#[used] #[no_mangle]
static FIRMWARE_VERSION_TAG: &[u8] =
    concat!("ATHENA_FIRMWARE_VERSION:", env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"), "\0").as_bytes();

#[used] #[no_mangle]
static FIRMWARE_MODEM_TAG: &[u8] =
    concat!("ATHENA_MODEM:", env!("ATHENA_MODEM"), "\0").as_bytes();

#[used] #[no_mangle]
static FIRMWARE_BOARD_TAG: &[u8] =
    concat!("ATHENA_BOARD:", env!("ATHENA_BOARD"), "\0").as_bytes();

// ---------------------------------------------------------------------------
// Application constants
// ---------------------------------------------------------------------------

const SLEEP_MINUTES: u64 = 30;
const INA3221_I2C_ADDR: u8 = 0x40;

const PHOTO_URL:       &str = "http://128.140.94.191/data/photo";
const SENSOR_DATA_URL: &str = "http://128.140.94.191/data/sensor";

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("Athena booting up with version {}", FIRMWARE_VERSION);
    info!("Version tags are: {:?} {:?} {:?}", FIRMWARE_VERSION_TAG, FIRMWARE_MODEM_TAG, FIRMWARE_BOARD_TAG);

    let (jpeg_quality, brightness_threshold) = load_config();

    // ── Internal temperature sensor ─────────────────────────────────────────
    let mut temp_sensor: temperature_sensor_handle_t = std::ptr::null_mut();
    let temp_sensor_config = temperature_sensor_config_t {
        range_min: 10,
        range_max: 50,
        clk_src: soc_periph_temperature_sensor_clk_src_t_TEMPERATURE_SENSOR_CLK_SRC_DEFAULT,
        ..Default::default()
    };
    let mut esp32_temp: f32 = 0.0;
    unsafe {
        temperature_sensor_install(&temp_sensor_config, &mut temp_sensor);
        temperature_sensor_enable(temp_sensor);
        temperature_sensor_get_celsius(temp_sensor, &mut esp32_temp);
    }

    // ── Shared power telemetry ───────────────────────────────────────────────
    let power_data     = Arc::new(power::PowerData::default());
    let task_running   = Arc::new(AtomicBool::new(true));

    // ── Peripherals ──────────────────────────────────────────────────────────
    let peripherals = match Peripherals::take() {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to take peripherals: {:?}, rebooting…", e);
            thread::sleep(Duration::from_secs(2));
            unsafe { esp_idf_sys::esp_restart(); }
        }
    };

    // ── GPIO: sleep signal and LED ───────────────────────────────────────────
    let mut sleep_signal_pin = board::pin(board::pins::SLEEP_SIGNAL)
        .and_then(|p| PinDriver::output(p).ok());

    if let Some(ref mut pin) = sleep_signal_pin {
        info!("Setting sleep signal high.");
        let _ = pin.set_high();
    }

    let mut led_pin = board::pin(board::pins::LED)
        .and_then(|p| PinDriver::output(p).ok());

    if let Some(ref mut pin) = led_pin {
        let _ = pin.set_low();
    }

    // ── INA3221 power monitor ────────────────────────────────────────────────
    if let (Some(sda), Some(scl)) = (
        board::pin(board::pins::I2C_SDA),
        board::pin(board::pins::I2C_SCL),
    ) {
        if let Ok(i2c) = I2cDriver::new(
            peripherals.i2c0,
            sda, scl,
            &i2c::I2cConfig::new().baudrate(Hertz(400_000)),
        ) {
            power::spawn_monitoring_task(
                INA3221::new(i2c, INA3221_I2C_ADDR),
                power_data.clone(),
                task_running.clone(),
            );
        }
    }

    // ── ESP event loop (needed for WiFi) ─────────────────────────────────────
    let _sysloop = match EspSystemEventLoop::take() {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to take ESP event loop: {:?}, rebooting…", e);
            thread::sleep(Duration::from_secs(2));
            unsafe { esp_idf_sys::esp_restart(); }
        }
    };

    // ── Modem ────────────────────────────────────────────────────────────────
    let mut gsm_module: Option<Box<dyn modem::Modem>> = None;

    #[cfg(feature = "modem-wifi")]
    let mut gsm_module: Option<Box<dyn modem::Modem>> = {
        const WIFI_SSID: &str = env!("WIFI_SSID");
        const WIFI_PASS: &str = env!("WIFI_PASS");
        match wifi::wifi(WIFI_SSID, WIFI_PASS, peripherals.modem, _sysloop) {
            Ok(wifi) => {
                info!("WiFi connected");
                Some(Box::new(WifiModem::new(wifi)))
            }
            Err(e) => {
                warn!("WiFi connection failed: {:?}, continuing without network", e);
                None
            }
        }
    };

    #[cfg(not(feature = "modem-wifi"))]
    if let (Some(tx), Some(rx), Some(sleep), Some(pwr)) = (
        board::pin(board::pins::GSM_TX),
        board::pin(board::pins::GSM_RX),
        board::pin(board::pins::GSM_SLP),
        board::pin(board::pins::GSM_PWR),
    ) {
        gsm_module = match UartDriver::new(
            peripherals.uart1,
            tx, rx,
            Option::<esp_idf_hal::gpio::AnyIOPin>::None,
            Option::<esp_idf_hal::gpio::AnyIOPin>::None,
            &UartConfig::new().baudrate(Hertz(115200)),
        ) {
            Ok(uart) => {
                let sleep_pin = PinDriver::output(sleep).ok();
                match PinDriver::output(pwr) {
                    Ok(power_pin) => Some(Box::new(Modem::new(uart, power_pin, sleep_pin))),
                    Err(e) => {
                        warn!("Failed to init modem power pin: {:?}", e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("Failed to init modem UART: {:?}", e);
                None
            }
        };
    } else {
        warn!("Not all GSM pins are configured for this board.");
    }

    // ── Camera ───────────────────────────────────────────────────────────────
    let camera = camera::init(jpeg_quality);

    // ── Device identity (MAC address) ────────────────────────────────────────
    let mac_string = unsafe {
        let mut mac = [0u8; 6];
        esp_idf_sys::esp_efuse_mac_get_default(mac.as_mut_ptr());
        format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])
    };

    // ── Main loop ────────────────────────────────────────────────────────────
    loop {
        let (image_data, brightness) = match camera {
            Some(ref cam) => {
                // Take a few pictures the let the camera's auto-exposition settle
                info!("Taking 10 frames over 2,5 seconds to let auto-exposure adjust");
                for _loop in 1..10 {
                    if let Some(frame) = cam.get_framebuffer() {
                        cam.return_framebuffer(frame);
                    }
                    thread::sleep(Duration::from_millis(250));
                }

                match cam.get_framebuffer() {
                    Some(frame) => {
                        let image_data = frame.data();
                        info!("Photo captured: {} bytes", image_data.len());
                        cam.return_framebuffer(frame);
                        (Some(image_data), camera::scene_brightness(cam))
                    }
                    None => {
                        error!("Failed to capture photo");
                        (None, 0.0)
                    }
                }
            }
            None => {
                (None, 0.0)
            }
        };

        if let Some(ref mut modem) = gsm_module {
            if let Err(e) = modem.initialize_network("simbase") {
                info!("Network init: {:?} (may already be up)", e);
            }

            let modem_voltage = modem.battery_voltage().unwrap_or_else(|e| {
                warn!("Battery voltage unavailable: {}", e);
                0.0
            });
            info!("Modem battery voltage: {:.3}V", modem_voltage);

            let signal_quality = modem.signal_quality().unwrap_or_else(|e| {
                warn!("Signal quality unavailable: {}", e);
                0
            });
            info!("Signal quality: {} dBm", signal_quality);

            // ── Sensor data POST ─────────────────────────────────
            let json_data = build_sensor_json(
                &power_data, modem_voltage, image_data.map_or_else(|| 0, |d| d.len()), esp32_temp, signal_quality
            );
            let headers = build_headers(&mac_string, "application/json");

            match modem.http_post(SENSOR_DATA_URL, json_data.as_bytes(), &headers) {
                Ok(resp) => {
                    handle_config_response(&resp, jpeg_quality, brightness_threshold);
                    handle_ota_response(&resp, &mac_string, modem);
                }
                Err(e) => warn!("Sensor data POST failed: {:?}", e),
            }

            if let Some(data) = image_data {
                info!("Scene brightness: {:.4}", brightness);
                if brightness <= 0.0 || brightness > brightness_threshold as f32 {
                    let photo_headers = build_headers(&mac_string, "image/jpeg");
                    if let Err(e) = modem.http_post(PHOTO_URL, data, &photo_headers) {
                        warn!("Photo POST failed: {:?}", e);
                    }
                } else {
                    info!("Scene too dark (brightness {:.4}), skipping photo upload.", brightness);
                }
            } else {
                info!("No photo to upload");
            }

            // ── Post-send energy summary ──────────────────────────
            let energy_json = format!(
                "{{\"brightness\":{{\"type\":\"brightness\",\"value\":{}}},\
                  \"battery\":{{\"type\":\"voltage\",\"value\":{:.3}}},\
                  \"board_energy_use\":{{\"type\":\"energy\",\"value\":{:.3}}}}}",
                brightness,
                power_data.ch3_voltage_v(),
                power_data.ch3_energy_as(),
            );
            let headers = build_headers(&mac_string, "application/json");
            if let Err(e) = modem.http_post(SENSOR_DATA_URL, energy_json.as_bytes(), &headers) {
                warn!("Energy summary POST failed: {:?}", e);
            }

            info!("Power data: {:?}", power_data);
        }

        info!("Sleeping for {} minute(s).", SLEEP_MINUTES);

        if let Some(ref mut pin) = led_pin {
            let _ = pin.set_high();
        }
        if let Some(ref mut pin) = sleep_signal_pin {
            info!("Setting sleep signal low.");
            let _ = pin.set_low();
        }

        task_running.store(false, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        thread::sleep(Duration::from_secs(60 * SLEEP_MINUTES));
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build common request headers, with `content_type` as the Content-Type value.
fn build_headers<'a>(mac: &'a str, content_type: &'a str) -> [(&'a str, &'a str); 5] {
    [
        ("Content-Type",      content_type),
        ("X-Board-Id",        mac),
        ("X-Firmware-Version", FIRMWARE_VERSION),
        ("X-Firmware-Modem",  env!("ATHENA_MODEM")),
        ("X-Firmware-Board",  env!("ATHENA_BOARD")),
    ]
}

/// Serialise current power measurements to the server's JSON format.
fn build_sensor_json(
    pd:          &power::PowerData,
    modem_v:     f32,
    photo_bytes: usize,
    temperature: f32,
    signal_quality: i32,
) -> String {
    format!(
        "{{\"battery\":{{\"type\":\"voltage\",\"value\":{:.3}}},\
          \"board_supply\":{{\"type\":\"voltage\",\"value\":{:.3}}},\
          \"battery_charging\":{{\"type\":\"voltage\",\"value\":{:.3}}},\
          \"board_current\":{{\"type\":\"current\",\"value\":{:.3}}},\
          \"battery_charging_current\":{{\"type\":\"current\",\"value\":{:.3}}},\
          \"supply_current\":{{\"type\":\"current\",\"value\":{:.3}}},\
          \"quectel_voltage\":{{\"type\":\"voltage\",\"value\":{:.3}}},\
          \"photo_size\":{{\"type\":\"generic\",\"value\":{}}},\
          \"temperature\":{{\"type\":\"temperature\",\"value\":{:.1}}},\
          \"signal\":{{\"type\":\"signal\",\"value\":{}}}}}",
        pd.ch3_voltage_v(),
        pd.ch1_voltage_v(),
        pd.ch2_voltage_v(),
        pd.ch3_current_a(),
        pd.ch2_current_a(),
        pd.ch1_current_a(),
        modem_v,
        photo_bytes,
        temperature,
        signal_quality,
    )
}

/// Reads settings from NVRAM
fn load_config() -> (i32, u32) {
    let mut jpeg_quality = 5;
    let mut brightness_threshold = 10;

    if let Ok(nvs_partition) = EspDefaultNvsPartition::take() {
        if let Ok(nvs) = EspNvs::new(nvs_partition, "athena", true) {
            jpeg_quality = nvs.get_i32("jpeg_quality").unwrap_or(None).unwrap_or(5);
            info!("JPEG quality loaded from config: {}", jpeg_quality);

            brightness_threshold = nvs.get_u32("rightn_thr").unwrap_or(None).unwrap_or(10);
            info!("Brightness threshold loaded from config: {}", brightness_threshold);
        }
    }

    (jpeg_quality, brightness_threshold)
}

/// Stores settings into NVRAM
fn handle_config_response(resp: &modem::HttpResponse, jpeg_quality_current: i32, brightness_threshold_current: u32) {
    if let Ok(nvs_partition) = EspDefaultNvsPartition::take() {
        if let Ok(nvs) = EspNvs::new(nvs_partition, "athena", true) {
            if let Some(jpeg_quality_new) = resp.header("X-Jpeg-Quality") {
                let jpeg_quality_parsed = jpeg_quality_new.parse::<i32>().unwrap_or(10);
                if jpeg_quality_parsed != jpeg_quality_current {
                    let _ = nvs.set_i32("jpeg_quality", jpeg_quality_parsed);
                    info!("JPEG quality saved from response: {}", jpeg_quality_parsed);
                }
            }

            if let Some(brightness_threshold_new) = resp.header("X-Brightness-Threshold") {
                let brightness_threshold_parsed = brightness_threshold_new.parse::<u32>().unwrap_or(10);
                if brightness_threshold_parsed != brightness_threshold_current {
                    let _ = nvs.set_u32("brightn_thr", brightness_threshold_parsed);
                    info!("Brightness threshold saved from response: {}", brightness_threshold_parsed);
                }
            }
        }
    }
}

/// Check the sensor-data POST response for an OTA update directive and apply it.
fn handle_ota_response(resp: &modem::HttpResponse, mac: &str, modem: &mut Box<dyn modem::Modem>) {
    if let (Some(fw_url), Some(fw_sha256), Some(fw_version)) = (
        resp.header("X-Firmware-Update"),
        resp.header("X-Firmware-SHA256"),
        resp.header("X-Firmware-Version"),
    ) {
        let remote_v  = fw_version.parse::<f32>().unwrap_or(0.0);
        let current_v = FIRMWARE_VERSION.parse::<f32>().unwrap_or(0.0);

        info!("OTA available: version {} at '{}'", fw_version, fw_url);

        if remote_v <= current_v {
            info!("OTA version {} is not newer than current {}, skipping.", remote_v, current_v);
            return;
        }

        let get_headers = [
            ("X-Board-Id",        mac),
            ("X-Firmware-Version", FIRMWARE_VERSION),
        ];

        match modem.http_get(fw_url, &get_headers) {
            Ok(fw_resp) => {
                let board_tag = concat!("ATHENA_BOARD:", env!("ATHENA_BOARD")).as_bytes();
                let modem_tag = concat!("ATHENA_MODEM:", env!("ATHENA_MODEM")).as_bytes();

                if !ota::check_firmware_compatibility(&fw_resp.body, board_tag) {
                    warn!("OTA firmware is not for this board, skipping.");
                } else if !ota::check_firmware_compatibility(&fw_resp.body, modem_tag) {
                    warn!("OTA firmware is not for this modem, skipping.");
                } else if let Err(e) = ota::install_firmware(&fw_resp.body, fw_sha256) {
                    warn!("OTA install failed: {:?}", e);
                }
            }
            Err(e) => warn!("OTA download failed: {:?}", e),
        }
    }
}
