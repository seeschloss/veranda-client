use esp_idf_hal::gpio::AnyIOPin;

pub fn pin(n: i32) -> AnyIOPin {
    unsafe { AnyIOPin::new(n) }
}

// Board: Seeed XIAO ESP32S3
#[cfg(feature = "board-xiao")]
pub mod pins {
    pub const LED: i32 = 21;
    pub const I2C_SDA: i32 = 5;
    pub const I2C_SCL: i32 = 6;
    pub const GSM_TX: i32 = 2;
    pub const GSM_RX: i32 = 4;
    pub const GSM_PWR: i32 = 3;
    pub const GSM_SLP: i32 = 9;
    pub const CAM_XCLK:  i32 = 10;
    pub const CAM_SDA:   i32 = 40;
    pub const CAM_SCL:   i32 = 39;
    pub const CAM_D0:    i32 = 15;
    pub const CAM_D1:    i32 = 17;
    pub const CAM_D2:    i32 = 18;
    pub const CAM_D3:    i32 = 16;
    pub const CAM_D4:    i32 = 14;
    pub const CAM_D5:    i32 = 12;
    pub const CAM_D6:    i32 = 11;
    pub const CAM_D7:    i32 = 48;
    pub const CAM_VSYNC: i32 = 38;
    pub const CAM_HREF:  i32 = 47;
    pub const CAM_PCLK:  i32 = 13;
}

// Board: ESP32-S3-WROOM
#[cfg(feature = "board-wroom")]
pub mod pins {
    pub const LED: i32 = 2;
    pub const I2C_SDA: i32 = 48;
    pub const I2C_SCL: i32 = 45;
    pub const GSM_TX: i32 = 21;
    pub const GSM_RX: i32 = 47;
    pub const GSM_PWR: i32 = -1;
    pub const GSM_SLP: i32 = -1;
    pub const CAM_XCLK:  i32 = 15;
    pub const CAM_SDA:   i32 = 4;
    pub const CAM_SCL:   i32 = 5;
    pub const CAM_D0:    i32 = 11;
    pub const CAM_D1:    i32 = 9;
    pub const CAM_D2:    i32 = 8;
    pub const CAM_D3:    i32 = 10;
    pub const CAM_D4:    i32 = 12;
    pub const CAM_D5:    i32 = 18;
    pub const CAM_D6:    i32 = 17;
    pub const CAM_D7:    i32 = 16;
    pub const CAM_VSYNC: i32 = 6;
    pub const CAM_HREF:  i32 = 7;
    pub const CAM_PCLK:  i32 = 13;
}

