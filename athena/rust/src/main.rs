use esp_idf_hal::{
    gpio::PinDriver,
    i2c::{self, I2cDriver},
    peripherals::Peripherals,
    uart::{UartConfig, UartDriver},
    units::Hertz,
};
use esp_idf_sys::{self as _, *};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use log::*;
use std::time::Duration;
use std::thread;
use std::sync::{Arc, Mutex};
use embedded_hal_bus::i2c::MutexDevice;
use anyhow::{anyhow, Result};

use core::sync::atomic::{AtomicBool, Ordering};

use chrono::NaiveDateTime;

use ina3221::INA3221;

use esp_idf_svc::eventloop::EspSystemEventLoop;

mod board;
mod camera;
mod modem;
mod ota;
mod power;
mod logger;

#[cfg(any(feature = "modem-simcom", feature = "modem-quectel"))]
mod ppp;

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

const INA3221_I2C_ADDR: u8 = 0x40;
const NRF_I2C_ADDR: u8 = 0x42;

const PHOTO_URL: &str = "http://128.140.94.191/data/photo";
const SENSOR_DATA_URL: &str = "http://128.140.94.191/data/sensor";
const LOG_URL: &str = "http://128.140.94.191/data/log";

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    esp_idf_sys::link_patches();

    let log_state = Arc::new(Mutex::new(logger::LogState {
        network_time: None,
        boot_time_us: 0,
    }));

    let log_buffer: &'static logger::LogBuffer = Box::leak(Box::new(logger::LogBuffer::new(500, log_state.clone())));

    if let Err(e) = log::set_logger(log_buffer) {
        println!("Logger error: {}", e);
    }
    log::set_max_level(log::LevelFilter::Info);

    info!("Athena booting up with version {}", FIRMWARE_VERSION);
    info!("Version tags are: {} / {} / {}",
        String::from_utf8_lossy(FIRMWARE_VERSION_TAG),
        String::from_utf8_lossy(FIRMWARE_MODEM_TAG),
        String::from_utf8_lossy(FIRMWARE_BOARD_TAG)
    );

    let (mut jpeg_quality, mut brightness_threshold, mut sleep_minutes) = load_config();

    let esp32_temp = get_internal_temperature();

    // ── GPIO: sleep signal and LED ───────────────────────────────────────────
    let mut sleep_signal_pin = board::pin(board::pins::SLEEP_SIGNAL)
        .and_then(|p| PinDriver::output(p).ok());

    if let Some(ref mut pin) = sleep_signal_pin {
        info!("Setting sleep signal high.");
        let _ = pin.set_high();
    }

    // ── Shared power telemetry ───────────────────────────────────────────────
    let power_data = Arc::new(power::PowerData::default());
    let task_running = Arc::new(AtomicBool::new(true));

    // ── Peripherals ──────────────────────────────────────────────────────────
    let peripherals = match Peripherals::take() {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to take peripherals: {:?}, rebooting…", e);
            thread::sleep(Duration::from_secs(2));
            unsafe { esp_idf_sys::esp_restart(); }
        }
    };

    // ── LED ──────────────────────────────────────────────────────────────────
    let mut led_pin = board::pin(board::pins::LED)
        .and_then(|p| PinDriver::output(p).ok());

    if let Some(ref mut pin) = led_pin {
        let _ = pin.set_low();
    }

    // ── Shared I2C bus (i2c0) ────────────────────────────────────────────────
    let i2c_bus: Option<&'static Mutex<I2cDriver>> =
        if let (Some(sda), Some(scl)) = (
            board::pin(board::pins::I2C_SDA),
            board::pin(board::pins::I2C_SCL),
        ) {
            match I2cDriver::new(
                peripherals.i2c0,
                sda, scl,
                &i2c::I2cConfig::new().baudrate(Hertz(400_000)),
            ) {
                Ok(drv) => {
                    info!("I2C bus initialized");
                    Some(Box::leak(Box::new(Mutex::new(drv))))
                },
                Err(e) => {
                    warn!("Failed to init shared I2C bus: {:?}", e);
                    None
                }
            }
        } else {
            warn!("Failed to init I2C bus: pins not defined");
            None
        };

    if let Some(bus) = i2c_bus {
        info!("Spawning monitoring task");
        power::spawn_monitoring_task(
            INA3221::new(MutexDevice::new(bus), INA3221_I2C_ADDR),
            power_data.clone(),
            task_running.clone(),
        );
    } else {
        info!("No I2C bus available, power will not be monitored");
    }

    let mut gsm_module: Option<Box<dyn modem::Modem>> = None;

    let _sysloop = match EspSystemEventLoop::take() {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to take ESP event loop: {:?}, rebooting…", e);
            thread::sleep(Duration::from_secs(2));
            unsafe { esp_idf_sys::esp_restart(); }
        }
    };

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
            rx, tx,
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

    info!("Power data: {:?}", power_data);

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
            None => (None, 0.0),
        };

        if let Some(ref mut modem) = gsm_module {
            let mut retries = 3;
            let mut result = Err(anyhow!("Network not initialised yet"));

            while retries > 0 {
                if let Err(e) = modem.initialize_network(
                    "simbase",
                    Duration::from_secs(30),
                    Duration::from_secs(120),
                ) {
                    info!("Network init error: {:?}, {} retries remaining", e, retries);
                    let _ = modem.reboot();

                    result = Err(e);
                    retries -= 1;
                } else {
                    result = Ok(());
                    break;
                }
            };

            if let Err(e) = result {
                warn!("Could not initialise network, giving up: {}", e);
            } else {
                let network_time = modem.network_time().unwrap_or_else(|e| {
                    warn!("Network time unavailable: {}", e);
                    NaiveDateTime::default()
                });
                info!("Network time: {}", network_time);

                if let Ok(mut state) = log_buffer.state.lock() {
                    state.network_time = Some(network_time);
                    state.boot_time_us = unsafe { esp_idf_sys::esp_timer_get_time() };
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

                let json_data = build_sensor_json(
                    &power_data,
                    modem_voltage,
                    image_data.map_or_else(|| 0, |d| d.len()),
                    esp32_temp,
                    signal_quality,
                );
                let headers = build_headers(&mac_string, "application/json");

                match modem.http_post(SENSOR_DATA_URL, json_data.as_bytes(), &headers) {
                    Ok(resp) => {
                        handle_config_response(&resp, &mut jpeg_quality, &mut brightness_threshold, &mut sleep_minutes);
                        match handle_ota_response(&resp, &mac_string, modem) {
                            Ok(Some(new_sleep_minutes)) => {
                                sleep_minutes = new_sleep_minutes;
                                info!("OTA successful, continuing process with early reboot ({} minute)", sleep_minutes);
                            },
                            Ok(None) => {
                                info!("No OTA needed");
                            },
                            Err(e) => {
                                warn!("OTA error: {}", e);
                            }
                        }
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
                        info!("Scene too dark (brightness {:.4} / threshold {:.4}), skipping photo upload.", brightness, brightness_threshold);
                    }
                } else {
                    info!("No photo to upload");
                }

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

                let log_body = log_buffer.drain().join("\n");
                let log_headers = build_headers(&mac_string, "text/plain");
                let _ = modem.http_post(LOG_URL, log_body.as_bytes(), &log_headers);

                let _ = modem.power_off();
            }
        }

        info!("Sleeping for {} minute(s).", sleep_minutes);

        if let Some(ref mut pin) = led_pin {
            let _ = pin.set_high();
        }

        if let Some(bus) = i2c_bus {
            info!("Notifying nRF to sleep for {} minutes", sleep_minutes);
            let mut drv = bus.lock().expect("I2C mutex poisoned");
            notify_nrf(&mut *drv, sleep_minutes);
            thread::sleep(Duration::from_millis(500));
        } else {
            info!("No I2C bus available, will not use I2C to notify nRF sleep duration");
        }

        if let Some(ref mut pin) = sleep_signal_pin {
            info!("Setting sleep signal low.");
            let _ = pin.set_low();
        }

        task_running.store(false, Ordering::SeqCst);
        thread::sleep(Duration::from_secs(60 * sleep_minutes as u64));
    }
}

// ---------------------------------------------------------------------------
// nRF sleep notification
// ---------------------------------------------------------------------------

fn notify_nrf<I>(i2c: &mut I, sleep_minutes: u32)
where
    I: embedded_hal::i2c::I2c,
{
    let secs = (sleep_minutes * 60).min(u16::MAX as u32) as u16;
    let payload = [0x17_u8, 0x89, (secs >> 8) as u8, secs as u8];
    match i2c.write(NRF_I2C_ADDR, &payload) {
        Ok(_)  => info!("Notified nRF: sleep {} min ({} s).", sleep_minutes, secs),
        Err(e) => warn!("Failed to notify nRF over I2C: {:?}", e),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_headers<'a>(mac: &'a str, content_type: &'a str) -> [(&'a str, &'a str); 5] {
    [
        ("Content-Type",       content_type),
        ("X-Board-Id",         mac),
        ("X-Firmware-Version", FIRMWARE_VERSION),
        ("X-Firmware-Modem",   env!("ATHENA_MODEM")),
        ("X-Firmware-Board",   env!("ATHENA_BOARD")),
    ]
}

fn build_sensor_json(
    pd: &power::PowerData,
    modem_v: f32,
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

fn load_config() -> (i32, u32, u32) {
    let mut jpeg_quality = 5;
    let mut brightness_threshold = 10;
    let mut sleep_minutes = 30;

    if let Ok(nvs_partition) = EspDefaultNvsPartition::take() {
        if let Ok(nvs) = EspNvs::new(nvs_partition, "athena", true) {
            jpeg_quality = nvs.get_i32("jpeg_quality").unwrap_or(None).unwrap_or(5);
            info!("JPEG quality loaded from config: {}", jpeg_quality);

            brightness_threshold = nvs.get_u32("brightn_thr").unwrap_or(None).unwrap_or(10);
            info!("Brightness threshold loaded from config: {}", brightness_threshold);

            sleep_minutes = nvs.get_u32("sleep_minutes").unwrap_or(None).unwrap_or(30);
            info!("Sleep time loaded from config: {}", sleep_minutes);
        }
    }

    (jpeg_quality, brightness_threshold, sleep_minutes)
}

fn handle_config_response(
    resp: &modem::HttpResponse,
    jpeg_quality_current: &mut i32,
    brightness_threshold_current: &mut u32,
    sleep_minutes_current: &mut u32,
) {
    if let Ok(nvs_partition) = EspDefaultNvsPartition::take() {
        if let Ok(nvs) = EspNvs::new(nvs_partition, "athena", true) {
            info!("Processing config for headers: {:?}", resp.headers);

            if let Some(v) = resp.header("X-Jpeg-Quality") {
                let parsed = v.parse::<i32>().unwrap_or(10);
                info!("JPEG quality retrieved from response: {}", parsed);
                if parsed != *jpeg_quality_current {
                    let _ = nvs.set_i32("jpeg_quality", parsed);
                    info!("JPEG quality saved from response: {}", parsed);
                    *jpeg_quality_current = parsed;
                }
            }
            if let Some(v) = resp.header("X-Brightness-Threshold") {
                let parsed = v.parse::<u32>().unwrap_or(10);
                info!("Brightness threshold retrieved from response: {}", parsed);
                if parsed != *brightness_threshold_current {
                    let _ = nvs.set_u32("brightn_thr", parsed);
                    info!("Brightness threshold saved from response: {}", parsed);
                    *brightness_threshold_current = parsed;
                }
            }
            if let Some(v) = resp.header("X-Sleep-Minutes") {
                let parsed = v.parse::<u32>().unwrap_or(10);
                info!("Sleep time retrieved from response: {}", parsed);
                if parsed != *sleep_minutes_current {
                    let _ = nvs.set_u32("sleep_minutes", parsed);
                    info!("Sleep time saved from response: {}", parsed);
                    *sleep_minutes_current = parsed;
                }
            }
        }
    }
}

fn handle_ota_response (
    resp: &modem::HttpResponse,
    mac: &str,
    modem: &mut Box<dyn modem::Modem>,
) -> Result<Option<u32>> {
    if let (Some(fw_url), Some(fw_sha256), Some(fw_version)) = (
        resp.header("X-Firmware-Update"),
        resp.header("X-Firmware-SHA256"),
        resp.header("X-Firmware-Version"),
    ) {
        let mut must_update = false;

        if let Some(remote_v_parts) = fw_version.split_once('.') {
            if let Some(current_v_parts) = FIRMWARE_VERSION.split_once('.') {
                let remote_v_maj  = remote_v_parts.0.parse::<u32>().unwrap_or(0);
                let remote_v_min  = remote_v_parts.1.parse::<u32>().unwrap_or(0);

                let current_v_maj  = current_v_parts.0.parse::<u32>().unwrap_or(0);
                let current_v_min  = current_v_parts.1.parse::<u32>().unwrap_or(0);

                if remote_v_maj > current_v_maj {
                    must_update = true;
                } else if remote_v_maj == current_v_maj && remote_v_min > current_v_min {
                    must_update = true;
                } else {
                    must_update = false;
                }
            }
        }

        info!("OTA available: version {} at '{}'", fw_version, fw_url);

        if !must_update {
            info!("OTA version {} is not newer than current {}, marking running firmware as safe.", fw_version, FIRMWARE_VERSION);
            ota::mark_update_as_valid();
            return Ok(None);
        }

        let get_headers = [
            ("X-Board-Id", mac),
            ("X-Firmware-Version", FIRMWARE_VERSION),
        ];

        match modem.http_get(fw_url, &get_headers) {
            Ok(fw_resp) => {
                info!("OTA update size: {} bytes. Headers: {:?}", fw_resp.body.len(), fw_resp.headers);

                let board_tag = concat!("ATHENA_BOARD:", env!("ATHENA_BOARD")).as_bytes();
                let modem_tag = concat!("ATHENA_MODEM:", env!("ATHENA_MODEM")).as_bytes();

                if !ota::check_firmware_compatibility(&fw_resp.body, board_tag) {
                    warn!("OTA firmware is not for this board, skipping.");
                } else if !ota::check_firmware_compatibility(&fw_resp.body, modem_tag) {
                    warn!("OTA firmware is not for this modem, skipping.");
                } else if let Err(e) = ota::install_firmware(&fw_resp.body, fw_sha256) {
                    warn!("OTA install failed: {:?}", e);
                } else {
                    return Ok(Some(1));
                }
            }
            Err(e) => warn!("OTA download failed: {:?}", e),
        }
    }

    Err(anyhow!("Could not perform OTA"))
}

#[cfg(feature = "board-esp32cam")]
fn get_internal_temperature() -> f32 {
    0.0
}

#[cfg(not(feature = "board-esp32cam"))]
fn get_internal_temperature() -> f32 {
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

    esp32_temp
}
