use esp_idf_hal::{
    peripherals::Peripherals,
    uart::{UartDriver, UartConfig},
    gpio::PinDriver,
    units::Hertz,
};
use esp_idf_sys::{self as _, *};
use esp_camera_rs::Camera;
use esp_idf_svc::eventloop::EspSystemEventLoop;

use esp_idf_hal::i2c::{self, I2cDriver};

use esp_idf_sys::{
    soc_periph_temperature_sensor_clk_src_t_TEMPERATURE_SENSOR_CLK_SRC_DEFAULT,
    temperature_sensor_config_t, temperature_sensor_enable, temperature_sensor_get_celsius,
    temperature_sensor_handle_t, temperature_sensor_install,
};

use log::*;
use std::time::Duration;
use std::thread;

//mod dcim;
mod board;

mod ota;

mod modem;

#[cfg(feature = "modem-wifi")]
mod wifi;

#[cfg(feature = "modem-wifi")]
use wifi::WifiModem;

#[cfg(feature = "modem-simcom")]
use simcom::SimcomModule as Modem;

#[cfg(feature = "modem-simcom")]
mod simcom;

#[cfg(feature = "modem-quectel")]
use quectel::QuectelModule as Modem;

#[cfg(feature = "modem-quectel")]
mod quectel;

//use dcim::{SDSPIHost, DCIM};

use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::sync::Arc;
use core::ffi::c_void;

use ina3221::{INA3221, OperatingMode, Voltage};

const FIRMWARE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"));

#[used]
#[no_mangle]
static FIRMWARE_VERSION_TAG: &[u8] = concat!("ATHENA_FIRMWARE_VERSION:", env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"), "\0").as_bytes();

#[used]
#[no_mangle]
static FIRMWARE_MODEM_TAG: &[u8] = concat!("ATHENA_MODEM:", env!("ATHENA_MODEM"), "\0").as_bytes();

#[used]
#[no_mangle]
static FIRMWARE_BOARD_TAG: &[u8] = concat!("ATHENA_BOARD:", env!("ATHENA_BOARD"), "\0").as_bytes();

// ESP32 sleeping time between pictures (should actually be powered down by the nRF)
const SLEEP_MINUTES: u64 = 30;

const INA3221_I2C_ADDR: u8 = 0x40;
const SHUNT_RESISTANCE: f32 = 0.1f32;   // 0.1 Ohm

const PHOTO_URL: &str = "http://128.140.94.191/data/photo";
const SENSOR_DATA_URL: &str = "http://128.140.94.191/data/sensor";

#[derive(Default, Debug)]
struct PowerData {
    ch1_voltage: AtomicU32,
    ch1_current: AtomicU32,
    ch1_energy: AtomicU32,
    ch2_voltage: AtomicU32,
    ch2_current: AtomicU32,
    ch2_energy: AtomicU32,
    ch3_voltage: AtomicU32,
    ch3_current: AtomicU32,
    ch3_energy: AtomicU32,
}

extern "C" fn ina3221_monitoring_task(arg: *mut c_void) {
    let boxed: Box<(INA3221<I2cDriver<'_>>, Arc<PowerData>, Arc<AtomicBool>)> = unsafe { Box::from_raw(arg as *mut _) };
    let (mut ina, shared_box, shared_bool_task_running) = *boxed;

    let _ = ina.set_channels_enabled(&[true, true, true]);
    let _ = ina.set_mode(OperatingMode::Continuous);

    let sleep_time = Duration::from_millis(10);

    let mut ch1_max_current = 0.0;
    let mut ch2_max_current = 0.0;
    let mut ch3_max_current = 0.0;

    let mut ch1_energy = 0.0;
    let mut ch2_energy = 0.0;
    let mut ch3_energy = 0.0;

    loop {
        if !shared_bool_task_running.load(Ordering::SeqCst) {
            let _ = ina.set_mode(OperatingMode::PowerDown);
        }

        let vin_bus_voltage = ina.get_bus_voltage(1).unwrap_or(Voltage::from_micro_volts(0));
        let vbat_in_bus_voltage = ina.get_bus_voltage(2).unwrap_or(Voltage::from_micro_volts(0));
        let vbat_out_bus_voltage = ina.get_bus_voltage(3).unwrap_or(Voltage::from_micro_volts(0));
        let vin_shunt_voltage = ina.get_shunt_voltage(1).unwrap_or(Voltage::from_micro_volts(0));
        let vbat_in_shunt_voltage = ina.get_shunt_voltage(2).unwrap_or(Voltage::from_micro_volts(0));
        let vbat_out_shunt_voltage = ina.get_shunt_voltage(3).unwrap_or(Voltage::from_micro_volts(0));

        shared_box.ch1_voltage.store((1000.0 * (vin_bus_voltage + vin_shunt_voltage).volts()) as u32, Ordering::Relaxed);
        shared_box.ch2_voltage.store((1000.0 * (vbat_in_bus_voltage + vbat_in_shunt_voltage).volts()) as u32, Ordering::Relaxed);
        shared_box.ch3_voltage.store((1000.0 * (vbat_out_bus_voltage + vbat_out_shunt_voltage).volts()) as u32, Ordering::Relaxed);

        let ch1_current = vin_shunt_voltage.volts() / SHUNT_RESISTANCE;
        let ch2_current = vbat_in_shunt_voltage.volts() / SHUNT_RESISTANCE;
        let ch3_current = vbat_out_shunt_voltage.volts() / SHUNT_RESISTANCE;

        ch1_max_current = f32::max(ch1_max_current, ch1_current);
        ch2_max_current = f32::max(ch2_max_current, ch2_current);
        ch3_max_current = f32::max(ch3_max_current, ch3_current);

        shared_box.ch1_current.store((1000.0 * ch1_max_current) as u32, Ordering::Relaxed);
        shared_box.ch2_current.store((1000.0 * ch2_max_current) as u32, Ordering::Relaxed);
        shared_box.ch3_current.store((1000.0 * ch3_max_current) as u32, Ordering::Relaxed);

        ch1_energy += (ch1_current * sleep_time.as_millis() as f32) / 1000.0;
        ch2_energy += (ch2_current * sleep_time.as_millis() as f32) / 1000.0;
        ch3_energy += (ch3_current * sleep_time.as_millis() as f32) / 1000.0;

        shared_box.ch1_energy.store((1000.0 * ch1_energy) as u32, Ordering::Relaxed);
        shared_box.ch2_energy.store((1000.0 * ch2_energy) as u32, Ordering::Relaxed);
        shared_box.ch3_energy.store((1000.0 * ch3_energy) as u32, Ordering::Relaxed);

        //info!("CPU1 power data: {:?}", shared_box);

        thread::sleep(sleep_time);
    }
}

fn spawn_ina3221_monitoring_task(ina: INA3221<I2cDriver<'_>>, shared: Arc<PowerData>, shared_bool_task_running: Arc<AtomicBool>) {
    let boxed = Box::new((ina, shared, shared_bool_task_running));

    unsafe {
        xTaskCreatePinnedToCore(
            Some(ina3221_monitoring_task),
            b"ina3221_monitoring_task\0".as_ptr() as *const _,
            4096,          // stack size (bytes)
            Box::into_raw(boxed) as *mut _,
            5,             // priority
            core::ptr::null_mut(),
            1,             // CORE 1
        );
    }
}

fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("Athena booting up with version {}", FIRMWARE_VERSION);

    let mut temp_sensor: temperature_sensor_handle_t = std::ptr::null_mut();
    let temp_sensor_config = temperature_sensor_config_t {
        range_min: 10,
        range_max: 50,
        clk_src: soc_periph_temperature_sensor_clk_src_t_TEMPERATURE_SENSOR_CLK_SRC_DEFAULT,
        ..Default::default()
    };

    let mut esp32_internal_temperature: f32 = 0.0;

    unsafe {
        temperature_sensor_install(&temp_sensor_config, &mut temp_sensor);
        temperature_sensor_enable(temp_sensor);
        temperature_sensor_get_celsius(temp_sensor, &mut esp32_internal_temperature);
    }


    let shared_box = Arc::new(PowerData::default());
    let shared_bool_task_running = Arc::new(AtomicBool::new(true));

    let peripherals = match Peripherals::take() {
        Ok(peripherals) => peripherals,
        Err(e) => {
            println!("Failed to take peripherals: {:?}, resetting board...", e);
            thread::sleep(Duration::from_secs(2));
            unsafe { esp_idf_sys::esp_restart(); }
        }
    };

    let mut sleep_signal_pin = board::pin(board::pins::SLEEP_SIGNAL)
        .and_then(|p| PinDriver::output(p).ok());

    if let Some(ref mut sleep_signal_pin) = sleep_signal_pin {
        info!("Setting sleep signal to high.");
        let _ = sleep_signal_pin.set_high();
    }

    let mut led_pin = board::pin(board::pins::LED)
        .and_then(|p| PinDriver::output(p).ok());

    if let Some(ref mut led_pin) = led_pin {
        let _ = led_pin.set_low();
    }

    if let (Some(sda), Some(scl)) = (board::pin(board::pins::I2C_SDA), board::pin(board::pins::I2C_SCL)) {
        if let Ok(i2c) = i2c::I2cDriver::new(
            peripherals.i2c0,
            sda,
            scl,
            &i2c::I2cConfig::new().baudrate(Hertz(400_000)),
        ) {
            spawn_ina3221_monitoring_task(INA3221::new(i2c, INA3221_I2C_ADDR), shared_box.clone(), shared_bool_task_running.clone());
        }
    }

    let _sysloop = match EspSystemEventLoop::take() {
        Ok(sysloop) => sysloop,
        Err(e) => {
            println!("Failed to take ESP event loop: {:?}, retrying in 1 second...", e);
            thread::sleep(Duration::from_secs(2));
            unsafe { esp_idf_sys::esp_restart(); }
        }
    };

    let mut gsm_module: Option<Box<dyn modem::Modem>> = None;

    #[cfg(feature = "modem-wifi")]
    let mut gsm_module: Option<Box<dyn modem::Modem>> = {
        const WIFI_SSID: &str = env!("WIFI_SSID");
        const WIFI_PASS: &str = env!("WIFI_PASS");
        match wifi::wifi(WIFI_SSID, WIFI_PASS, peripherals.modem, _sysloop) {
            Ok(wifi) => {
                info!("WiFi connected, using WifiModem");
                Some(Box::new(WifiModem::new(wifi)))
            }
            Err(e) => {
                info!("WiFi connection failed: {:?}, continuing without network", e);
                None
            }
        }
    };

    #[cfg(not(feature = "modem-wifi"))]
    if let (Some(tx), Some(rx), Some(sleep), Some(pwr)) = (board::pin(board::pins::GSM_TX), board::pin(board::pins::GSM_RX), board::pin(board::pins::GSM_SLP), board::pin(board::pins::GSM_PWR)) {
        gsm_module = match UartDriver::new(
            peripherals.uart1,
            tx,
            rx,
            Option::<esp_idf_hal::gpio::AnyIOPin>::None,
            Option::<esp_idf_hal::gpio::AnyIOPin>::None,
            &UartConfig::new().baudrate(esp_idf_hal::units::Hertz(115200)),
        ) {
            Ok(uart) => {
                let sleep_pin = match PinDriver::output(sleep) {
                    Ok(sleep_pin) => Some(sleep_pin),
                    Err(_) => None,
                };

                match PinDriver::output(pwr) {
                    Ok(power_pin) => Some(Box::new(Modem::new(uart, power_pin, sleep_pin))),
                    Err(e) => {
                        println!("Failed to initialize 4G module power pin: {:?}, continuing without 4G", e);
                        None
                    },
                }
            },
            Err(e) => {
                println!("Failed to initialize 4G module UART: {:?}, continuing without 4G", e);
                None
            },
        };
    } else {
        println!("No GSM module as not all pins are available.");
    }

    let mut camera: Option<Camera> = None;

    if let (
        Some(xclk), Some(sda), Some(scl),
        Some(d0), Some(d1), Some(d2), Some(d3),
        Some(d4), Some(d5), Some(d6), Some(d7),
        Some(vsync), Some(href), Some(pclk),
    ) = (
        board::pin(board::pins::CAM_XCLK),
        board::pin(board::pins::CAM_SDA),
        board::pin(board::pins::CAM_SCL),
        board::pin(board::pins::CAM_D0),
        board::pin(board::pins::CAM_D1),
        board::pin(board::pins::CAM_D2),
        board::pin(board::pins::CAM_D3),
        board::pin(board::pins::CAM_D4),
        board::pin(board::pins::CAM_D5),
        board::pin(board::pins::CAM_D6),
        board::pin(board::pins::CAM_D7),
        board::pin(board::pins::CAM_VSYNC),
        board::pin(board::pins::CAM_HREF),
        board::pin(board::pins::CAM_PCLK),
        ) {
        camera = match Camera::new(
            xclk,
            sda,
            scl,
            d0,
            d1,
            d2,
            d3,
            d4,
            d5,
            d6,
            d7,
            vsync,
            href,
            pclk,
            10_000_000, // ~8 MHz to ~20 MHz
            19,   // 4 (best) to 19 (worst)
            2,
            camera::camera_grab_mode_t_CAMERA_GRAB_LATEST,
            camera::framesize_t_FRAMESIZE_QSXGA,
        ) {
            Ok(camera) => {
                let _ = camera.sensor().set_hmirror(true);
                Some(camera)
            },
            Err(e) => {
                println!("Failed to initialize camera: {:?}, continuing without images", e);
                None
            },
        };
    } else {
        println!("Camera not available as not all pins are there.");
    }


    let mac_string = unsafe {
        let mut base_mac = [0u8; 6];
        esp_idf_sys::esp_efuse_mac_get_default(base_mac.as_mut_ptr());

        format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                base_mac[0], base_mac[1], base_mac[2],
                base_mac[3], base_mac[4], base_mac[5])
    };

    //let mut sdcard: Option<SDSPIHost> = None;
    /*{
        let mut host = SDSPIHost::new();
        match host.mount() {
            Ok(()) => Some(host),
            Err(e) => {
                info!("Could not mount SD card: {:?}", e);
                None
            }
        }
    };*/

    loop {
        if let Some(ref camera) = camera {
            match camera.get_framebuffer() {
                Some(frame) => {
                    let image_data = frame.data();
                    info!("Photo captured successfully: {} bytes", image_data.len());

                    /*
                    if let Some(ref mut sdcard) = sdcard {
                        let next_number = sdcard.next_number("ATH_");
                        let next_filename = format!("ATH_{}.JPG", next_number);
                        if let Err(e) = sdcard.write(&image_data, next_filename.as_str()) {
                            info!("Could not write to SD card: {:?}", e);
                        }
                    }
                    */

                    if let Some(ref mut gsm_module) = gsm_module {
                        //if let Err(e) = gsm_module.wake() {
                        //    info!("Could not power on 4G module: {:?} (but maybe it's already powered on)", e);
                        //}

                        if let Err(e) = gsm_module.initialize_network("simbase") {
                            info!("Could not initialise 4G module: {:?} (but maybe it's already initialised)", e);
                        }

                        let modem_voltage = match gsm_module.battery_voltage() {
                            Ok(voltage) => voltage,
                            Err(e) => {
                                info!("Couldn't read 4G module voltage: {}", e);
                                0.0
                            },
                        };
                        info!("Battery voltage according to the 4G module is: {}", modem_voltage);

                        let headers = [
                            ("Content-Type", "application/json"),
                            ("X-Board-Id", mac_string.as_str()),
                            ("X-Firmware-Version", FIRMWARE_VERSION),
                            ("X-Firmware-Modem", env!("ATHENA_MODEM")),
                            ("X-Firmware-Board", env!("ATHENA_BOARD")),
                        ];

                        let json_data = format!("{{ \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {} }}",
                            "battery", format!("{{ \"type\":\"voltage\",\"value\":{} }}", shared_box.ch3_voltage.load(Ordering::Relaxed) as f32 / 1000.0),
                            "board_supply", format!("{{ \"type\":\"voltage\",\"value\":{} }}", shared_box.ch1_voltage.load(Ordering::Relaxed) as f32 / 1000.0),
                            "battery_charging", format!("{{ \"type\":\"voltage\",\"value\":{} }}", shared_box.ch2_voltage.load(Ordering::Relaxed) as f32 / 1000.0),
                            "board_current", format!("{{ \"type\":\"current\",\"value\":{} }}", shared_box.ch3_current.load(Ordering::Relaxed) as f32 / 1000.0),
                            "battery_charging_current", format!("{{ \"type\":\"current\",\"value\":{} }}", shared_box.ch2_current.load(Ordering::Relaxed) as f32 / 1000.0),
                            "supply_current", format!("{{ \"type\":\"current\",\"value\":{} }}", shared_box.ch1_current.load(Ordering::Relaxed) as f32 / 1000.0),
                            "quectel_voltage", format!("{{ \"type\":\"voltage\",\"value\":{} }}", modem_voltage),
                            "photo_size", format!("{{ \"type\":\"generic\",\"value\":{} }}", image_data.len()),
                            "temperature", format!("{{ \"type\":\"temperature\",\"value\":{} }}", esp32_internal_temperature),
                        );

                        info!("CPU0 power data: {:?}", shared_box);

                        match gsm_module.http_post(SENSOR_DATA_URL, json_data.as_bytes(), &headers) {
                            Ok(response) => {
                                if let (Some(fw_url), Some(fw_sha256), Some(fw_version)) = (
                                    response.header("X-Firmware-Update"),
                                    response.header("X-Firmware-SHA256"),
                                    response.header("X-Firmware-Version"),
                                ) {
                                    let parsed_version_update = fw_version.parse::<f32>().unwrap_or(0.0);
                                    let parsed_version_current = FIRMWARE_VERSION.parse::<f32>().unwrap_or(0.0);

                                    info!("OTA update signalled, firmware version {} (parsed: {}) at '{}'", fw_version, parsed_version_update, fw_url);

                                    if parsed_version_update > parsed_version_current {
                                        info!("Update is newer than our version ({}), performing OTA update", parsed_version_current);
                                        let headers = [
                                            ("X-Board-Id", mac_string.as_str()),
                                            ("X-Firmware-Version", FIRMWARE_VERSION),
                                        ];
                                        match gsm_module.http_get(&fw_url, &headers) {
                                            Ok(fw_response) => {
                                                let expected_board_tag = concat!("ATHENA_BOARD:", env!("ATHENA_BOARD")).as_bytes();
                                                let expected_modem_tag = concat!("ATHENA_MODEM:", env!("ATHENA_MODEM")).as_bytes();

                                                if !ota::check_firmware_compatibility(&fw_response.body, expected_board_tag) {
                                                    info!("OTA firmware is not for this board, skipping");
                                                } else if !ota::check_firmware_compatibility(&fw_response.body, expected_modem_tag) {
                                                    info!("OTA firmware is not for this modem, skipping");
                                                } else {
                                                    if let Err(e) = ota::install_firmware(&fw_response.body, &fw_sha256) {
                                                        info!("OTA failed: {:?}", e);
                                                    }
                                                }
                                            }
                                            Err(e) => info!("OTA download failed: {:?}", e),
                                        }
                                    } else {
                                        info!("Update is not newer than our version ({}), no need to update", parsed_version_current);
                                    }
                                } else {
                                    info!("Could not retrieve firmware update info, somehow?");
                                }
                            }
                            Err(e) => info!("Could not send data through 4G module: {:?}", e),
                        }

                        let headers = [
                            ("Content-Type", "image/jpeg"),
                            ("X-Board-Id", mac_string.as_str()),
                            ("X-Firmware-Version", FIRMWARE_VERSION),
                            ("X-Firmware-Modem", env!("ATHENA_MODEM")),
                            ("X-Firmware-Board", env!("ATHENA_BOARD")),
                        ];

                        let brightness = scene_brightness(camera);

                        if brightness <= 0.0 || brightness > 10.0 {
                            info!("Sending photo (brightness: {})", brightness);
                            if let Err(e) = gsm_module.http_post(PHOTO_URL, image_data, &headers) {
                                info!("Could not send photo through 4G module: {:?}", e);
                            }
                        } else {
                            info!("Not sending photo because it's too dark (brightness: {})", brightness);
                        }

                        let headers = [
                            ("Content-Type", "application/json"),
                            ("X-Board-Id", mac_string.as_str()),
                            ("X-Firmware-Version", FIRMWARE_VERSION),
                            ("X-Firmware-Modem", env!("ATHENA_MODEM")),
                            ("X-Firmware-Board", env!("ATHENA_BOARD")),
                        ];

                        let json_data = format!("{{ \"{}\": {}, \"{}\": {}, \"{}\": {} }}",
                            "brightness", format!("{{ \"type\":\"brightness\",\"value\":{} }}", brightness),
                            "battery", format!("{{ \"type\":\"voltage\",\"value\":{} }}", shared_box.ch3_voltage.load(Ordering::Relaxed) as f32 / 1000.0),
                            "board_energy_use", format!("{{ \"type\":\"energy\",\"value\":{} }}", shared_box.ch3_energy.load(Ordering::Relaxed) as f32 / 1000.0),
                        );

                        if let Err(e) = gsm_module.http_post(
                            SENSOR_DATA_URL,
                            json_data.as_bytes(),
                            &headers) {
                            info!("Could not send data through 4G module: {:?}", e);
                        }

                        info!("CPU0 power data: {:?}", shared_box);

                        //let _ = gsm_module.sleep();
                    }

                    camera.return_framebuffer(frame);
                }
                None => {
                    error!("Failed to capture photo");
                }
            }
        }

        info!("Sleeping for {} minute(s), now.", SLEEP_MINUTES);

        if let Some(ref mut led_pin) = led_pin {
            let _ = led_pin.set_high();
        }

        if let Some(ref mut sleep_signal_pin) = sleep_signal_pin {
            info!("Setting sleep signal to low.");
            let _ = sleep_signal_pin.set_low();
        }

        shared_bool_task_running.clone().store(false, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(100));
        thread::sleep(Duration::from_secs(60 * SLEEP_MINUTES));
        /*
        unsafe {
            esp_sleep_enable_timer_wakeup(SLEEP_MINUTES * 60 * 1_000_000);
            esp_deep_sleep_start();
        }
        */
    }
}

fn scene_brightness(camera: &Camera) -> f32 {
    let sensor = camera.sensor();

    // AEC exposure value: 20-bit across 0x3500[3:0], 0x3501[7:0], 0x3502[7:4]
    let exp_hi  = sensor.get_reg(0x3500, 0x0F) as u32;
    let exp_mid = sensor.get_reg(0x3501, 0xFF) as u32;
    let exp_lo  = sensor.get_reg(0x3502, 0xF0) as u32;
    let exposure = (exp_hi << 12) | (exp_mid << 4) | (exp_lo >> 4);

    // AGC gain: 10-bit across 0x350A[1:0] and 0x350B[7:0]
    let gain_hi = sensor.get_reg(0x350A, 0x03) as u32;
    let gain_lo = sensor.get_reg(0x350B, 0xFF) as u32;
    let gain = (gain_hi << 8) | gain_lo;

    info!("AEC exposure={}, AGC gain={}", exposure, gain);

    (1200.0 * 800.0) / (gain as f32 * exposure as f32)
}

