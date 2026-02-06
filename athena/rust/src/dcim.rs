#![allow(dead_code)]

use log::info;
use std::ffi::{c_int, c_void};
use esp_idf_sys as _; // If using the `binstart` feature of `esp-idf-sys`, always keep this module imported
use esp_idf_sys::sdmmc_slot_config_t;
use esp_idf_sys::sdmmc_card_t;
use esp_idf_sys::sdmmc_host_t;
use esp_idf_hal::sys::{sdspi_device_config_t, spi_bus_config_t};
use anyhow::Result;

use std::fs;
use std::io::Write;

// SDMMC HOST
const SDMMC_SLOT_CONFIG_WIDTH : u8 = 1;
const SDMMC_SLOT_CONFIG_CLK : i32 = 7;
const SDMMC_SLOT_CONFIG_CMD : i32 = 9;
const SDMMC_SLOT_CONFIG_D0 : i32 = 8;
const SDMMC_SLOT_CONFIG_D1 : i32 = -1;
const SDMMC_SLOT_CONFIG_D2 : i32 = 3;
const SDMMC_SLOT_CONFIG_D3 : i32 = -1;
const SDMMC_SLOT_CONFIG_D4 : i32 = -1;
const SDMMC_SLOT_CONFIG_D5 : i32 = -1;
const SDMMC_SLOT_CONFIG_D6 : i32 = -1;
const SDMMC_SLOT_CONFIG_D7 : i32 = -1;
const SDMMC_HOST_FLAG_8BIT: u32 = 1 << 2;
const SDMMC_HOST_FLAG_4BIT: u32 = 1 << 1;
const SDMMC_HOST_FLAG_1BIT: u32 = 1 << 0;
const SDMMC_FREQ_HIGHSPEED: c_int = 40000; // SD High speed (limited by clock divider)
const SDMMC_FREQ_PROBING: c_int = 400; // SD/MMC probing speed
const SDMMC_FREQ_52M: c_int = 52000; // MMC 52MHz speed
const SDMMC_FREQ_26M: c_int = 26000; // MMC 26MHz speed

// SDSPI HOST
const SPI_HOST_ID: i32 = 1;
const SDMMC_FREQ_DEFAULT: c_int = 20000; // SD/MMC Default speed (limited by clock divider)
const SDMMC_HOST_FLAG_SPI : u32 = 1 << 3;
const SDMMC_HOST_FLAG_DEINIT_ARG : u32 = 1 << 5;
const SDSPI_SLOT_CONFIG_CMD: i32 = 9;   // GPIO9 MOSI
const SDSPI_SLOT_CONFIG_CLK: i32 = 7;   // GPIO7 SCLK
const SDSPI_SLOT_CONFIG_D0: i32 = 8;    // GPIO8 MISO
const SDSPI_SLOT_CONFIG_CS: i32 = 21;   // GPIO21 CS

const SDMMC_SLOT_NO_CD: esp_idf_sys::gpio_num_t = esp_idf_sys::gpio_num_t_GPIO_NUM_NC; // indicates that card detect line is not used
const SDMMC_SLOT_NO_WP: esp_idf_sys::gpio_num_t = esp_idf_sys::gpio_num_t_GPIO_NUM_NC; // indicates that write protect line is not used
const SDMMC_SLOT_WIDTH_DEFAULT: u8 = 0; // use the maximum possible width for the slot

const VFS_MOUNT_ALLOC_UNIT_SIZE: usize = 32 * 1024;

const MOUNT_POINT : &[u8] = b"/sdcard\0";

pub struct SDSPIHost {
    host: *mut sdmmc_host_t,
    card: esp_idf_hal::sys::sdmmc_card_t,
    sdspi_config: sdspi_device_config_t,
    spi_bus_config: spi_bus_config_t,
}

impl DCIM for SDSPIHost {
    fn mount(&mut self) -> Result<(), anyhow::Error> {
        let slot_config_ptr = &self.sdspi_config as *const sdspi_device_config_t;
        let mount_config = esp_idf_sys::esp_vfs_fat_sdmmc_mount_config_t {
            format_if_mount_failed: true,
            max_files: 5,
            allocation_unit_size: VFS_MOUNT_ALLOC_UNIT_SIZE,
            disk_status_check_enable: false,
        };

        let mut card_ptr = &mut self.card as *mut esp_idf_hal::sys::sdmmc_card_t;

        let mount_emmc = unsafe {
            let ret = esp_idf_hal::sys::spi_bus_initialize(
                SPI_HOST_ID as u32,
                &mut self.spi_bus_config,
                esp_idf_hal::sys::spi_common_dma_t_SPI_DMA_CH_AUTO
            );
            if ret != esp_idf_sys::ESP_OK {
                return Err(anyhow::anyhow!("Failed to initialize SPI bus"));
            }
            info!("spi_bus_initialize SPIxID: {}", SPI_HOST_ID);
            esp_idf_hal::sys::esp_vfs_fat_sdspi_mount(
                MOUNT_POINT.as_ptr() as *const u8,
                self.host,
                slot_config_ptr,
                &mount_config,
                &mut card_ptr,
            )
        };
        let mount_point_str = std::str::from_utf8(MOUNT_POINT).unwrap();
        match mount_emmc {
            esp_idf_sys::ESP_OK => {
                info!("SDSPI SD Card mounted successfully on {}", mount_point_str);
                Ok(())
            }
            e => {
                Err(anyhow::anyhow!("Failed to mount. {}", e))
            }
        }
    }

    fn unmount(&mut self) -> Result<(), anyhow::Error> {
        let card_unmount_result = unsafe { esp_idf_sys::esp_vfs_fat_sdcard_unmount(MOUNT_POINT.as_ptr() as *const u8, &mut self.card) };

        match card_unmount_result {
            esp_idf_sys::ESP_OK => {
                info!("eMMC/SD card unmounted successfully");
                Ok(())
            }
            _ => {
                Err(anyhow::anyhow!("Failed to mount"))
            }
        }
    }
}

impl SDSPIHost {
    pub fn new() -> SDSPIHost {
        SDSPIHost {
            host : Box::into_raw(Box::new(esp_idf_hal::sys::sdmmc_host_t {
                flags: SDMMC_HOST_FLAG_SPI | SDMMC_HOST_FLAG_DEINIT_ARG,
                slot: SPI_HOST_ID,
                max_freq_khz: SDMMC_FREQ_DEFAULT,        
                io_voltage: 3.3,
                init: Some(esp_idf_hal::sys::sdspi_host_init),
                set_bus_width: None,
                get_bus_width: None,
                set_bus_ddr_mode: None,
                set_card_clk: Some(esp_idf_hal::sys::sdspi_host_set_card_clk),
                set_cclk_always_on: None,
                do_transaction: Some(esp_idf_hal::sys::sdspi_host_do_transaction),
                io_int_enable: Some(esp_idf_hal::sys::sdspi_host_io_int_enable),
                io_int_wait: Some(esp_idf_hal::sys::sdspi_host_io_int_wait),
                command_timeout_ms: 0,
                get_real_freq: Some(esp_idf_hal::sys::sdspi_host_get_real_freq),
                __bindgen_anon_1: esp_idf_hal::sys::sdmmc_host_t__bindgen_ty_1 {
                    deinit_p: Some(esp_idf_hal::sys::sdspi_host_remove_device),
                },        
            })),
            card: esp_idf_hal::sys::sdmmc_card_t::default(),
            sdspi_config: esp_idf_sys::sdspi_device_config_t {
                host_id: SPI_HOST_ID as u32,
                gpio_cs: SDSPI_SLOT_CONFIG_CS,
                gpio_cd: -1,
                gpio_wp: -1,
                gpio_int: -1 },
            spi_bus_config: esp_idf_hal::sys::spi_bus_config_t {
                __bindgen_anon_1: esp_idf_hal::sys::spi_bus_config_t__bindgen_ty_1 {
                    mosi_io_num: SDSPI_SLOT_CONFIG_CMD,
                },
                __bindgen_anon_2: esp_idf_hal::sys::spi_bus_config_t__bindgen_ty_2 {
                    miso_io_num: SDSPI_SLOT_CONFIG_D0,
                },
                sclk_io_num: SDSPI_SLOT_CONFIG_CLK,
                __bindgen_anon_3: esp_idf_hal::sys::spi_bus_config_t__bindgen_ty_3 {
                    quadwp_io_num: -1,
                },
                __bindgen_anon_4: esp_idf_hal::sys::spi_bus_config_t__bindgen_ty_4 {
                    quadhd_io_num: -1,
                },
                data4_io_num: -1,
                data5_io_num: -1,
                data6_io_num: -1,
                data7_io_num: -1,
                max_transfer_sz: 4000,
                flags: 0,
                intr_flags: 0,
                isr_cpu_id: 0,
            },
        }
    }

}

pub struct EMMCHost {
    host: *mut sdmmc_host_t,
    card: *mut sdmmc_card_t,
    slot_config: sdmmc_slot_config_t,
}

impl DCIM for EMMCHost {
    fn mount(&mut self) -> Result<(), anyhow::Error> {
        let slot_ptr = &self.slot_config as *const sdmmc_slot_config_t;

        self.card = std::ptr::null_mut();
        let card_ptr = &mut self.card as *mut *mut sdmmc_card_t;

        let mount_config = esp_idf_sys::esp_vfs_fat_sdmmc_mount_config_t {
            disk_status_check_enable: false,
            format_if_mount_failed: false,
            max_files: 5,
            allocation_unit_size: VFS_MOUNT_ALLOC_UNIT_SIZE,
        };

        let card_mount_result = unsafe {
            esp_idf_sys::esp_vfs_fat_sdmmc_mount(
                MOUNT_POINT.as_ptr() as *const u8,
                self.host,
                slot_ptr as *const c_void,
                &mount_config,
                card_ptr,
            )
        };

        match card_mount_result {
            esp_idf_sys::ESP_OK => {
                info!("eMMC/SD card mounted successfully {}", std::str::from_utf8(MOUNT_POINT).unwrap());
                Ok(())
            }
            _ => {
                Err(anyhow::anyhow!("Failed to mount {}", std::str::from_utf8(MOUNT_POINT).unwrap()))
            }
        }   
    }

    fn unmount(&mut self) -> Result<(), anyhow::Error> {
        let card_unmount_result = unsafe { esp_idf_sys::esp_vfs_fat_sdmmc_unmount() };

        match card_unmount_result {
            esp_idf_sys::ESP_OK => {
                info!("eMMC/SD card unmounted successfully");
                Ok(())
            }
            _ => {
                Err(anyhow::anyhow!("Failed to mount"))
            }
        }   
    }
}

impl EMMCHost {
    pub fn new() -> EMMCHost {
        EMMCHost {
            host : Box::into_raw(Box::new(esp_idf_sys::sdmmc_host_t {
                flags: SDMMC_HOST_FLAG_1BIT,
                slot: 0,
                max_freq_khz: SDMMC_FREQ_52M,
                io_voltage: 3.3,
                init: Some(esp_idf_sys::sdmmc_host_init),
                set_bus_width: Some(esp_idf_sys::sdmmc_host_set_bus_width),
                get_bus_width: Some(esp_idf_sys::sdmmc_host_get_slot_width),
                set_bus_ddr_mode: Some(esp_idf_sys::sdmmc_host_set_bus_ddr_mode),
                set_card_clk: Some(esp_idf_sys::sdmmc_host_set_card_clk),
                set_cclk_always_on: Some(esp_idf_sys::sdmmc_host_set_cclk_always_on),
                do_transaction: Some(esp_idf_sys::sdmmc_host_do_transaction),
                io_int_enable: Some(esp_idf_sys::sdmmc_host_io_int_enable),
                io_int_wait: Some(esp_idf_sys::sdmmc_host_io_int_wait),
                command_timeout_ms: 0,
                get_real_freq: Some(esp_idf_sys::sdmmc_host_get_real_freq),
                __bindgen_anon_1: esp_idf_sys::sdmmc_host_t__bindgen_ty_1 {
                    deinit: Some(esp_idf_sys::sdmmc_host_deinit),
                },
            })),
            card: std::ptr::null_mut(),
            slot_config: esp_idf_sys::sdmmc_slot_config_t {
                width: SDMMC_SLOT_CONFIG_WIDTH,
                clk: SDMMC_SLOT_CONFIG_CLK,
                cmd: SDMMC_SLOT_CONFIG_CMD,
                d0: SDMMC_SLOT_CONFIG_D0,
                d1: SDMMC_SLOT_CONFIG_D1,
                d2: SDMMC_SLOT_CONFIG_D2,
                d3: SDMMC_SLOT_CONFIG_D3,
                d4: SDMMC_SLOT_CONFIG_D4,
                d5: SDMMC_SLOT_CONFIG_D5,
                d6: SDMMC_SLOT_CONFIG_D6,
                d7: SDMMC_SLOT_CONFIG_D7,
                __bindgen_anon_1: esp_idf_sys::sdmmc_slot_config_t__bindgen_ty_1 {
                    gpio_cd: -1,
                },
                __bindgen_anon_2: esp_idf_sys::sdmmc_slot_config_t__bindgen_ty_2 {
                    gpio_wp: -1,
                },
                flags: 0,
            },
        }
    }
}

pub trait DCIM {
    fn mount(&mut self) -> Result<(), anyhow::Error>;
    fn unmount(&mut self) -> Result<(), anyhow::Error>;

    fn _next_number(&mut self, prefix: &str) -> Option<i32> {
        let mut current = 0;

        let directory: String = std::str::from_utf8(MOUNT_POINT).unwrap().trim_end_matches("\0").to_string() + "/ATHENA";

        let read_dir_iter = std::fs::read_dir(directory).ok()?;

        for entry in read_dir_iter {
            if let Ok(entry) = entry {
                info!("Found file...");
                let filename = entry.file_name().into_string().ok()?;
                info!("Found file: {}", filename);
                let remainder = filename.strip_prefix(&prefix)?;
                info!("Remainder: {}", remainder);
                let (number_str, _) = remainder.split_once(".")?;
                info!("Number: {}", number_str);
                let number = number_str.parse::<i32>().ok()?;
                info!("Parsed number: {}", number);
                current = std::cmp::max(current, number);
            }
        }

        Some(current + 1)
    }

    fn next_number(&mut self, prefix: &str) -> String {
        match self._next_number(prefix) {
            Some(next) => format!("{:06}", next),
            None => "000000".to_string(),
        }

    }

    fn write(&mut self, data: &[u8], filename: &str) -> Result<(), anyhow::Error> {
        let directory: String = std::str::from_utf8(MOUNT_POINT).unwrap().trim_end_matches("\0").to_string() + "/ATHENA";

        match fs::create_dir_all(directory.clone()) {
            Ok(_) => {
                info!("Directory created: {}", directory);
            },
            Err(_) => {
                info!("Could not create directory: {}", directory);
            },
        }

        let path = directory + "/" + filename;

        match fs::File::create(&path) {
            Ok(mut file) => {
                match file.write(data) {
                    Ok(_) => {
                        info!("{} bytes written successfully to {}", data.len(), path);
                        Ok(())
                    },
                    Err(e) => {
                        info!("Failed to write file `{}`: {:?}", path, e);
                        Err(anyhow::Error::msg("Failed to write file"))
                    }
                }
            },
            Err(e) => {
                info!("Failed to open file `{}`: {:?}", path, e);
                Err(anyhow::Error::msg("Failed to open file"))
            },
        }
    }
}
