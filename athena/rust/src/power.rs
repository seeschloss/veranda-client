//! INA3221 power monitoring — runs as a background FreeRTOS task on Core 1.
//!
//! `PowerData` is an `Arc`-wrapped struct of atomics that the main task can
//! read at any time without locks.  All stored values are integer milli-units
//! (mV / mA / mAs) to sidestep atomic `f32` issues on Xtensa; divide by
//! 1000.0 to recover floating-point values.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::thread;

use esp_idf_hal::i2c::I2cDriver;
use esp_idf_sys::xTaskCreatePinnedToCore;
use ina3221::{INA3221, OperatingMode, Voltage};

/// Shunt resistor value in Ohms used for current measurement on all three channels.
pub const SHUNT_RESISTANCE: f32 = 0.1;

/// Shared power telemetry updated every ~10 ms by the monitoring task.
///
/// Channels:
///   - ch1 → board supply (VIN)
///   - ch2 → battery charging rail
///   - ch3 → battery output
#[derive(Default, Debug)]
pub struct PowerData {
    pub ch1_voltage: AtomicU32, // millivolts
    pub ch1_current: AtomicU32, // milliamps (peak since last reset)
    pub ch1_energy:  AtomicU32, // milliamp-seconds
    pub ch2_voltage: AtomicU32,
    pub ch2_current: AtomicU32,
    pub ch2_energy:  AtomicU32,
    pub ch3_voltage: AtomicU32,
    pub ch3_current: AtomicU32,
    pub ch3_energy:  AtomicU32,
}

impl PowerData {
    /// Convenience helpers to read channel values as floats (volts / amps / amp-seconds).
    pub fn ch1_voltage_v(&self)  -> f32 { self.ch1_voltage.load(Ordering::Relaxed) as f32 / 1000.0 }
    pub fn ch2_voltage_v(&self)  -> f32 { self.ch2_voltage.load(Ordering::Relaxed) as f32 / 1000.0 }
    pub fn ch3_voltage_v(&self)  -> f32 { self.ch3_voltage.load(Ordering::Relaxed) as f32 / 1000.0 }
    pub fn ch1_current_a(&self)  -> f32 { self.ch1_current.load(Ordering::Relaxed) as f32 / 1000.0 }
    pub fn ch2_current_a(&self)  -> f32 { self.ch2_current.load(Ordering::Relaxed) as f32 / 1000.0 }
    pub fn ch3_current_a(&self)  -> f32 { self.ch3_current.load(Ordering::Relaxed) as f32 / 1000.0 }
    pub fn ch3_energy_as(&self)  -> f32 { self.ch3_energy.load(Ordering::Relaxed)  as f32 / 1000.0 }
}

// ---------------------------------------------------------------------------
// FreeRTOS task (unsafe C ABI required by xTaskCreatePinnedToCore)
// ---------------------------------------------------------------------------

extern "C" fn ina3221_monitoring_task(arg: *mut c_void) {
    let boxed: Box<(INA3221<I2cDriver<'_>>, Arc<PowerData>, Arc<AtomicBool>)> =
        unsafe { Box::from_raw(arg as *mut _) };
    let (mut ina, data, task_running) = *boxed;

    let _ = ina.set_channels_enabled(&[true, true, true]);
    let _ = ina.set_mode(OperatingMode::Continuous);

    let period = Duration::from_millis(10);
    let zero   = Voltage::from_micro_volts(0);

    let mut ch1_peak = 0.0f32;
    let mut ch2_peak = 0.0f32;
    let mut ch3_peak = 0.0f32;
    let mut ch1_energy = 0.0f32;
    let mut ch2_energy = 0.0f32;
    let mut ch3_energy = 0.0f32;

    loop {
        if !task_running.load(Ordering::SeqCst) {
            let _ = ina.set_mode(OperatingMode::PowerDown);
        }

        // Read bus and shunt voltages for all three channels.
        let bus1   = ina.get_bus_voltage(1).unwrap_or(zero);
        let shunt1 = ina.get_shunt_voltage(1).unwrap_or(zero);
        let bus2   = ina.get_bus_voltage(2).unwrap_or(zero);
        let shunt2 = ina.get_shunt_voltage(2).unwrap_or(zero);
        let bus3   = ina.get_bus_voltage(3).unwrap_or(zero);
        let shunt3 = ina.get_shunt_voltage(3).unwrap_or(zero);

        // Total voltage = bus + shunt.
        data.ch1_voltage.store(((bus1 + shunt1).volts() * 1000.0) as u32, Ordering::Relaxed);
        data.ch2_voltage.store(((bus2 + shunt2).volts() * 1000.0) as u32, Ordering::Relaxed);
        data.ch3_voltage.store(((bus3 + shunt3).volts() * 1000.0) as u32, Ordering::Relaxed);

        // Current = Vshunt / Rshunt.
        let i1 = shunt1.volts() / SHUNT_RESISTANCE;
        let i2 = shunt2.volts() / SHUNT_RESISTANCE;
        let i3 = shunt3.volts() / SHUNT_RESISTANCE;

        // Store peak current since start.
        ch1_peak = f32::max(ch1_peak, i1);
        ch2_peak = f32::max(ch2_peak, i2);
        ch3_peak = f32::max(ch3_peak, i3);

        data.ch1_current.store((ch1_peak * 1000.0) as u32, Ordering::Relaxed);
        data.ch2_current.store((ch2_peak * 1000.0) as u32, Ordering::Relaxed);
        data.ch3_current.store((ch3_peak * 1000.0) as u32, Ordering::Relaxed);

        // Integrate energy (amp-seconds).
        let dt = period.as_millis() as f32 / 1000.0;
        ch1_energy += i1 * dt;
        ch2_energy += i2 * dt;
        ch3_energy += i3 * dt;

        data.ch1_energy.store((ch1_energy * 1000.0) as u32, Ordering::Relaxed);
        data.ch2_energy.store((ch2_energy * 1000.0) as u32, Ordering::Relaxed);
        data.ch3_energy.store((ch3_energy * 1000.0) as u32, Ordering::Relaxed);

        thread::sleep(period);
    }
}

/// Spawn the INA3221 monitoring task on Core 1, passing ownership of the
/// driver and the two shared `Arc`s into the task.
pub fn spawn_monitoring_task(
    ina:          INA3221<I2cDriver<'_>>,
    data:         Arc<PowerData>,
    task_running: Arc<AtomicBool>,
) {
    let boxed = Box::new((ina, data, task_running));
    unsafe {
        xTaskCreatePinnedToCore(
            Some(ina3221_monitoring_task),
            b"ina3221_task\0".as_ptr() as *const _,
            4096,
            Box::into_raw(boxed) as *mut _,
            5,
            core::ptr::null_mut(),
            1, // Core 1
        );
    }
}
