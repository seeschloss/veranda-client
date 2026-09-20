use esp_idf_hal::gpio::AnyIOPin;

pub fn pin(n: i8) -> Option<AnyIOPin<'static>> {
    if n < 0 {
        None
    } else {
        Some(unsafe { AnyIOPin::steal(n as u8) })
    }

}

// Board: Seeed XIAO ESP32S3
#[cfg(feature = "board-xiao")]
pub mod pins {
    pub const SLEEP_SIGNAL: i8 = 1;
    pub const LED: i8 = 21;
    pub const I2C_SDA: i8 = 5;
    pub const I2C_SCL: i8 = 6;
    pub const GSM_TX: i8 = 2;
    pub const GSM_RX: i8 = 4;
    pub const GSM_PWR: i8 = 3;
    pub const GSM_SLP: i8 = 9;
    pub const CAM_XCLK:  i8 = 10;
    pub const CAM_SDA:   i8 = 40;
    pub const CAM_SCL:   i8 = 39;
    pub const CAM_D0:    i8 = 15;
    pub const CAM_D1:    i8 = 17;
    pub const CAM_D2:    i8 = 18;
    pub const CAM_D3:    i8 = 16;
    pub const CAM_D4:    i8 = 14;
    pub const CAM_D5:    i8 = 12;
    pub const CAM_D6:    i8 = 11;
    pub const CAM_D7:    i8 = 48;
    pub const CAM_VSYNC: i8 = 38;
    pub const CAM_HREF:  i8 = 47;
    pub const CAM_PCLK:  i8 = 13;
}

// Board: ESP32-S3-WROOM
#[cfg(feature = "board-wroom")]
pub mod pins {
    pub const SLEEP_SIGNAL: i8 = 0;
    pub const LED: i8 = 2;
    pub const I2C_SDA: i8 = 39;
    pub const I2C_SCL: i8 = 38;
    pub const GSM_TX: i8 = 42;
    pub const GSM_RX: i8 = 40;
    pub const GSM_PWR: i8 = 41;
    pub const GSM_SLP: i8 = 21;
    pub const CAM_XCLK:  i8 = 15;
    pub const CAM_SDA:   i8 = 4;
    pub const CAM_SCL:   i8 = 5;
    pub const CAM_D0:    i8 = 11;
    pub const CAM_D1:    i8 = 9;
    pub const CAM_D2:    i8 = 8;
    pub const CAM_D3:    i8 = 10;
    pub const CAM_D4:    i8 = 12;
    pub const CAM_D5:    i8 = 18;
    pub const CAM_D6:    i8 = 17;
    pub const CAM_D7:    i8 = 16;
    pub const CAM_VSYNC: i8 = 6;
    pub const CAM_HREF:  i8 = 7;
    pub const CAM_PCLK:  i8 = 13;
}

// Board: ESP32-CAM
#[cfg(feature = "board-esp32cam")]
pub mod pins {
    pub const SLEEP_SIGNAL: i8 = 15;
    pub const LED: i8 = 4;
    pub const I2C_SDA: i8 = 12;
    pub const I2C_SCL: i8 = 13;
    pub const GSM_TX: i8 = 14;
    pub const GSM_RX: i8 = 2;
    pub const GSM_PWR: i8 = -1;
    pub const GSM_SLP: i8 = 16;
    pub const CAM_XCLK:  i8 = 0;
    pub const CAM_SDA:   i8 = 26;
    pub const CAM_SCL:   i8 = 27;
    pub const CAM_D0:    i8 = 5;
    pub const CAM_D1:    i8 = 18;
    pub const CAM_D2:    i8 = 19;
    pub const CAM_D3:    i8 = 21;
    pub const CAM_D4:    i8 = 36;
    pub const CAM_D5:    i8 = 39;
    pub const CAM_D6:    i8 = 34;
    pub const CAM_D7:    i8 = 35;
    pub const CAM_VSYNC: i8 = 25;
    pub const CAM_HREF:  i8 = 23;
    pub const CAM_PCLK:  i8 = 22;
}


