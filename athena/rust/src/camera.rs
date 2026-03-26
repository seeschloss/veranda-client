//! Camera initialisation and image analysis helpers.
//!
//! This module owns the camera configuration constants and the logic for
//! initialising the OV5640 sensor.  Framebuffer capture/return stays in the
//! main loop because the frame lifetime must match the send operation.

use esp_camera_rs::Camera;
use esp_idf_sys::camera;
use log::{info, error};

use crate::board;

// ---------------------------------------------------------------------------
// Camera configuration constants
// ---------------------------------------------------------------------------

/// XCLK frequency fed to the sensor (10–20 MHz is the supported range).
const XCLK_FREQ_HZ: i32 = 10_000_000;

/// Number of frame buffers to allocate in PSRAM.
const FB_COUNT: usize = 2;

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Initialise the OV5640 camera using pin assignments from `board::pins`.
///
/// Returns `None` if any required pin is absent (pin number 0 in board.rs)
/// or if `Camera::new` fails.
pub fn init(jpeg_quality: i32) -> Option<Camera<'static>> {
    let (
        xclk, sda, scl,
        d0, d1, d2, d3,
        d4, d5, d6, d7,
        vsync, href, pclk,
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
    );

    let (
        xclk, sda, scl,
        d0, d1, d2, d3,
        d4, d5, d6, d7,
        vsync, href, pclk,
    ) = match (
        xclk, sda, scl, d0, d1, d2, d3,
        d4, d5, d6, d7, vsync, href, pclk,
    ) {
        (
            Some(xclk), Some(sda), Some(scl),
            Some(d0), Some(d1), Some(d2), Some(d3),
            Some(d4), Some(d5), Some(d6), Some(d7),
            Some(vsync), Some(href), Some(pclk),
        ) => (xclk, sda, scl, d0, d1, d2, d3, d4, d5, d6, d7, vsync, href, pclk),
        _ => {
            error!("Camera unavailable: one or more pins not configured for this board.");
            return None;
        }
    };

    match Camera::new(
        xclk, sda, scl,
        d0, d1, d2, d3, d4, d5, d6, d7,
        vsync, href, pclk,
        XCLK_FREQ_HZ,
        jpeg_quality,
        FB_COUNT,
        camera::camera_grab_mode_t_CAMERA_GRAB_LATEST,
        camera::framesize_t_FRAMESIZE_QSXGA,
    ) {
        Ok(cam) => {
            // Apply sensor settings.
            let sensor = cam.sensor();
            let _ = sensor.set_hmirror(true);
            let _ = sensor.set_aec2(true);
            let _ = sensor.set_exposure_ctrl(true);
            info!("Camera initialised ({}×{}, quality {})", 
                  camera::framesize_t_FRAMESIZE_QSXGA as u32, 0, jpeg_quality);
            Some(cam)
        }
        Err(e) => {
            error!("Camera init failed: {:?}", e);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Image analysis
// ---------------------------------------------------------------------------

/// Estimate scene brightness from the OV5640's AEC/AGC register values.
///
/// Returns a dimensionless value roughly proportional to scene illumination.
/// A high value (> 10) means the sensor is using low gain / short exposure,
/// i.e. the scene is bright.  Values ≤ 0 or very small indicate a night-time
/// scene; the caller should decide whether to suppress upload.
///
/// Register layout (OV5640 datasheet):
///   - Exposure: 20-bit across 0x3500[3:0] | 0x3501[7:0] | 0x3502[7:4]
///   - Gain:     10-bit across 0x350A[1:0] | 0x350B[7:0]
pub fn scene_brightness(camera: &Camera) -> f32 {
    let sensor = camera.sensor();

    let exp_hi  = sensor.get_reg(0x3500, 0x0F) as u32;
    let exp_mid = sensor.get_reg(0x3501, 0xFF) as u32;
    let exp_lo  = sensor.get_reg(0x3502, 0xF0) as u32;
    let exposure = (exp_hi << 12) | (exp_mid << 4) | (exp_lo >> 4);

    let gain_hi = sensor.get_reg(0x350A, 0x03) as u32;
    let gain_lo = sensor.get_reg(0x350B, 0xFF) as u32;
    let gain    = (gain_hi << 8) | gain_lo;

    info!("OV5640: AEC exposure={}, AGC gain={}", exposure, gain);

    if gain == 0 || exposure == 0 {
        return 0.0;
    }

    (1200.0 * 800.0) / (gain as f32 * exposure as f32)
}
