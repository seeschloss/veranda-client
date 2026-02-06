use esp_idf_hal::{
    peripherals::Peripherals,
    uart::{UartDriver, UartConfig},
    gpio::PinDriver,
    units::Hertz,
};
use esp_idf_sys::{self as _, *};
use esp_camera_rs::Camera;
use esp_idf_svc::{
    http::client::{Configuration, EspHttpConnection},
    eventloop::EspSystemEventLoop,
};
use embedded_svc::http::client::{Client, Method};

use esp_idf_hal::i2c::{self, I2cDriver};

use log::*;
use std::time::Duration;
use std::thread;

mod dcim;
mod quectel;
mod wifi;

use dcim::{SDSPIHost, DCIM};
use quectel::QuectelModule;
use wifi::wifi;

use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::sync::Arc;
use core::ffi::c_void;

use ina3221::{INA3221, OperatingMode, Voltage};

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

    if let Ok(i2c) = i2c::I2cDriver::new(
        peripherals.i2c0,
        peripherals.pins.gpio5,
        peripherals.pins.gpio6,
        &i2c::I2cConfig::new().baudrate(Hertz(400_000)),
    ) {
        spawn_ina3221_monitoring_task(INA3221::new(i2c, INA3221_I2C_ADDR), shared_box.clone(), shared_bool_task_running.clone());
    }

    let _ = match EspSystemEventLoop::take() {
        Ok(sysloop) => sysloop,
        Err(e) => {
            println!("Failed to take ESP event loop: {:?}, retrying in 1 second...", e);
            thread::sleep(Duration::from_secs(2));
            unsafe { esp_idf_sys::esp_restart(); }
        }
    };

    let mut gsm_module = match UartDriver::new(
        peripherals.uart1,
        peripherals.pins.gpio2, // D1
        peripherals.pins.gpio4, // D3
        Option::<esp_idf_hal::gpio::AnyIOPin>::None,
        Option::<esp_idf_hal::gpio::AnyIOPin>::None,
        &UartConfig::new().baudrate(esp_idf_hal::units::Hertz(115200)),
    ) {
        Ok(uart) => {
            let sleep_pin = match PinDriver::output(peripherals.pins.gpio9) {
                Ok(sleep_pin) => Some(sleep_pin),
                Err(_) => None,
            };

            match PinDriver::output(peripherals.pins.gpio3) { // D2
                Ok(power_pin) => Some(QuectelModule::new(uart, power_pin, sleep_pin)),
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

    let camera = match Camera::new(
        peripherals.pins.gpio10,
        peripherals.pins.gpio40,
        peripherals.pins.gpio39,
        peripherals.pins.gpio15,
        peripherals.pins.gpio17,
        peripherals.pins.gpio18,
        peripherals.pins.gpio16,
        peripherals.pins.gpio14,
        peripherals.pins.gpio12,
        peripherals.pins.gpio11,
        peripherals.pins.gpio48,
        peripherals.pins.gpio38,
        peripherals.pins.gpio47,
        peripherals.pins.gpio13,
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

    let wifi: Option<bool> = None;
    /*
    let wifi = match wifi(
        "Arctica",
        "1407178914071789",
        peripherals.modem,
        sysloop
    ) {
        Ok(wifi) => {
            println!("WiFi connected");
            Some(wifi)
        },
        Err(e) => {
            println!("Failed to initialize WiFi: {:?}, doing without", e);
            None
        },
    };
        */

    let mac_string = unsafe {
        let mut base_mac = [0u8; 6];
        esp_idf_sys::esp_efuse_mac_get_default(base_mac.as_mut_ptr());

        format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                base_mac[0], base_mac[1], base_mac[2],
                base_mac[3], base_mac[4], base_mac[5])
    };

    let mut sdcard: Option<SDSPIHost> = None;
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

                    if let Some(ref mut sdcard) = sdcard {
                        let next_number = sdcard.next_number("ATH_");
                        let next_filename = format!("ATH_{}.JPG", next_number);
                        if let Err(e) = sdcard.write(&image_data, next_filename.as_str()) {
                            info!("Could not write to SD card: {:?}", e);
                        }
                    }

                    let content_length = format!("{}", image_data.len());
                    let headers = [
                        ("Content-Type", "image/jpeg"),
                        ("Content-Length", content_length.as_str()),
                        ("X-Board-Id", mac_string.as_str()),
                    ];

                    if wifi.is_some() {
                        match EspHttpConnection::new(&Configuration::default()) {
                            Ok(connection) => {
                                let mut client = Client::wrap(connection);
                                match client.request(Method::Post,PHOTO_URL.as_ref(), &headers) {
                                    Ok(mut request) => {
                                        if let Err(e) = request.write(image_data) {
                                            info!("Could not send image data though POST request: {:?}", e);
                                        }
                                        let _ = request.flush();
                                        match request.submit() {
                                            Ok(response) => {
                                                let status = response.status();
                                                info!("Response status: {:?}", status);
                                            },
                                            Err(e) => {
                                                info!("HTTP request error: {:?}", e);
                                            },
                                        }
                                    },
                                    Err(e) => {
                                        info!("Cannot create HTTP request: {:?}", e);
                                    }
                                }
                            },
                            Err(e) => {
                                info!("Cannot create HTTP connection: {:?}", e);
                            }
                        }
                    }

                    if let Some(ref mut gsm_module) = gsm_module {
                        if let Err(e) = gsm_module.wake() {
                            info!("Could not power on 4G module: {:?} (but maybe it's already powered on)", e);
                        }

                        if let Err(e) = gsm_module.initialize_network("simbase") {
                            info!("Could not initialise 4G module: {:?} (but maybe it's already initialised)", e);
                        }

                        let quectel_voltage = match gsm_module.battery_voltage() {
                            Ok(voltage) => voltage,
                            Err(e) => {
                                info!("Couldn't read 4G module voltage: {}", e);
                                0.0
                            },
                        };
                        info!("Battery voltage according to the 4G module is: {}", quectel_voltage);

                        let headers = [
                            ("Content-Type", "application/json"),
                            ("X-Board-Id", mac_string.as_str()),
                        ];

                        let json_data = format!("{{ \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {}, \"{}\": {} }}",
                            "battery", format!("{{ \"type\":\"voltage\",\"value\":{} }}", shared_box.ch3_voltage.load(Ordering::Relaxed) as f32 / 1000.0),
                            "board_supply", format!("{{ \"type\":\"voltage\",\"value\":{} }}", shared_box.ch1_voltage.load(Ordering::Relaxed) as f32 / 1000.0),
                            "battery_charging", format!("{{ \"type\":\"voltage\",\"value\":{} }}", shared_box.ch2_voltage.load(Ordering::Relaxed) as f32 / 1000.0),
                            "board_current", format!("{{ \"type\":\"current\",\"value\":{} }}", shared_box.ch3_current.load(Ordering::Relaxed) as f32 / 1000.0),
                            "battery_charging_current", format!("{{ \"type\":\"current\",\"value\":{} }}", shared_box.ch2_current.load(Ordering::Relaxed) as f32 / 1000.0),
                            "supply_current", format!("{{ \"type\":\"current\",\"value\":{} }}", shared_box.ch1_current.load(Ordering::Relaxed) as f32 / 1000.0),
                            "quectel_voltage", format!("{{ \"type\":\"voltage\",\"value\":{} }}", quectel_voltage),
                            "photo_size", format!("{{ \"type\":\"generic\",\"value\":{} }}", image_data.len()),
                        );

                        info!("CPU0 power data: {:?}", shared_box);

                        if let Err(e) = gsm_module.http_post(
                            SENSOR_DATA_URL,
                            json_data.as_bytes(),
                            &headers) {
                            info!("Could not send data through 4G module: {:?}", e);
                        }

                        let headers = [
                            ("Content-Type", "image/jpeg"),
                            ("X-Board-Id", mac_string.as_str()),
                        ];

                        if let Err(e) = gsm_module.http_post(PHOTO_URL, image_data, &headers) {
                            info!("Could not send photo through 4G module: {:?}", e);
                        }

                        let headers = [
                            ("Content-Type", "application/json"),
                            ("X-Board-Id", mac_string.as_str()),
                        ];

                        let json_data = format!("{{ \"{}\": {}, \"{}\": {} }}",
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

                        let _ = gsm_module.sleep();
                    }

                    camera.return_framebuffer(frame);
                }
                None => {
                    error!("Failed to capture photo");
                }
            }
        }

        info!("Sleeping for {} minute(s), now.", SLEEP_MINUTES);

        shared_bool_task_running.clone().store(false, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(100));
        //thread::sleep(Duration::from_secs(60 * SLEEP_MINUTES));
        unsafe {
            esp_sleep_enable_timer_wakeup(SLEEP_MINUTES * 60 * 1_000_000);
            esp_deep_sleep_start();
        }
    }
}

