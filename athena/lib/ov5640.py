import espcamera
import board
import busio

class Cam:
    def __init__(self):
        self.cam = espcamera.Camera(
            data_pins=board.CAM_DATA,
            external_clock_pin=board.CAM_XCLK,
            pixel_clock_pin=board.CAM_PCLK,
            vsync_pin=board.CAM_VSYNC,
            href_pin=board.CAM_HREF,
            pixel_format=espcamera.PixelFormat.JPEG,
            frame_size=espcamera.FrameSize.QSXGA,
            i2c=busio.I2C(board.CAM_SCL, board.CAM_SDA),
            external_clock_frequency=20_000_000,
            framebuffer_count=2,
            grab_mode=espcamera.GrabMode.LATEST)
        self.cam.hmirror = True

    def take(self):
        quality = 4
        denoise = 2
        img = None

        while img is None and quality <= 20:
            self.cam.quality = quality
            self.cam.denoise = denoise

            print(f"Trying to take picture with quality={self.cam.quality} and denoise={self.cam.denoise}")

            img = self.cam.take(2)

            # decrease quality (increase number) and
            # increase denoise for next round, as usually
            # if the picture could not be taken it's because
            # it was too big, more than 1 MB
            quality = int(quality * 1.5)
            denoise *= 2

        return img
