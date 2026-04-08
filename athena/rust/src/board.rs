use esp_idf_hal::gpio::AnyIOPin;

pub fn pin(n: u8) -> Option<AnyIOPin<'static>> {
    if n == 0 {
        None
    } else {
        Some(unsafe { AnyIOPin::steal(n) })
    }

}

// Board: Seeed XIAO ESP32S3
#[cfg(feature = "board-xiao")]
pub mod pins {
    pub const SLEEP_SIGNAL: u8 = 1;
    pub const LED: u8 = 21;
    pub const I2C_SDA: u8 = 5;
    pub const I2C_SCL: u8 = 6;
    pub const GSM_TX: u8 = 2;
    pub const GSM_RX: u8 = 4;
    pub const GSM_PWR: u8 = 3;
    pub const GSM_SLP: u8 = 9;
    pub const CAM_XCLK:  u8 = 10;
    pub const CAM_SDA:   u8 = 40;
    pub const CAM_SCL:   u8 = 39;
    pub const CAM_D0:    u8 = 15;
    pub const CAM_D1:    u8 = 17;
    pub const CAM_D2:    u8 = 18;
    pub const CAM_D3:    u8 = 16;
    pub const CAM_D4:    u8 = 14;
    pub const CAM_D5:    u8 = 12;
    pub const CAM_D6:    u8 = 11;
    pub const CAM_D7:    u8 = 48;
    pub const CAM_VSYNC: u8 = 38;
    pub const CAM_HREF:  u8 = 47;
    pub const CAM_PCLK:  u8 = 13;
}

// Board: ESP32-S3-WROOM
#[cfg(feature = "board-wroom")]
pub mod pins {
    pub const SLEEP_SIGNAL: u8 = 0;
    pub const LED: u8 = 2;
    pub const I2C_SDA: u8 = 39;
    pub const I2C_SCL: u8 = 38;
    pub const GSM_TX: u8 = 42;
    pub const GSM_RX: u8 = 40;
    pub const GSM_PWR: u8 = 41;
    pub const GSM_SLP: u8 = 21;
    pub const CAM_XCLK:  u8 = 15;
    pub const CAM_SDA:   u8 = 4;
    pub const CAM_SCL:   u8 = 5;
    pub const CAM_D0:    u8 = 11;
    pub const CAM_D1:    u8 = 9;
    pub const CAM_D2:    u8 = 8;
    pub const CAM_D3:    u8 = 10;
    pub const CAM_D4:    u8 = 12;
    pub const CAM_D5:    u8 = 18;
    pub const CAM_D6:    u8 = 17;
    pub const CAM_D7:    u8 = 16;
    pub const CAM_VSYNC: u8 = 6;
    pub const CAM_HREF:  u8 = 7;
    pub const CAM_PCLK:  u8 = 13;
}


