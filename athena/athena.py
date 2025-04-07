FIRMWARE_VERSION=1
API_KEY="0a77a9542e071d66767e07d99e54738f"
PLACE_ID=16

import microcontroller
import time
import alarm
import board

import busio
import digitalio
import analogio

import espcamera

from ec200a import *

adc_battery = analogio.AnalogIn(board.D8)
voltage_battery = adc_battery.value / 2**16 * 3.3 * 2
adc_psu = analogio.AnalogIn(board.D10)
voltage_supply = adc_psu.value / 2**16 * 3.3 * 2
esp_temperature = microcontroller.cpu.temperature

print(f"Battery voltage: {voltage_battery} V")
print(f"Supply voltage: {voltage_supply} V")
print(f"Temperature: {esp_temperature} °C")

pin_sleep_signal = digitalio.DigitalInOut(board.D9)
pin_sleep_signal.switch_to_output()
pin_sleep_signal.value = 0

pin_led = digitalio.DigitalInOut(board.LED)
pin_led.switch_to_output()
pin_led.value = 0

pin_power = digitalio.DigitalInOut(board.D2)
pin_power.switch_to_output()
pin_power.value = 1

uart = busio.UART(rx=board.D3, tx=board.D4, baudrate=115200)
http = HTTP_EC200A(uart, [("X-Api-Key", API_KEY), ("User-Agent", f"Athena/{FIRMWARE_VERSION} (CircuitPython, EC200A-EU)")])

if http.init_modem(25000):
    time.sleep(2)
    http.send_http_post_json("http://veranda.seos.fr/data/sensor", f'{{ "57": {voltage_battery}, "58": {voltage_supply}, "56": {esp_temperature} }}')
    time.sleep(1)
    if voltage_supply > 2:
        # Pas besoin de photo s'il fait nuit, on va dire
        cam = espcamera.Camera(
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
        cam.denoise = 2
        cam.quality = 5
        img = cam.take(2)

        if img is None:
            # On essaie de reprendre une image avec une qualité inférieure, au cas où
            cam.quality = 6
            img = cam.take(2)

        if img is None:
            # On essaie de reprendre une image avec une qualité inférieure, au cas où
            cam.quality = 10
            img = cam.take(2)

        if img is not None:
            print(f"Picture size: {len(img)}")
            try:
                http.send_file(f"http://veranda.seos.fr/data/place/{PLACE_ID}/photo", img)
            except:
                pin_led.value = 1
        else:
            print(f"Cannot take picture? {img}")
            pin_led.value = 1

    time.sleep(10)
else:
    print(f"Couldn't init modem")

print(f"Time to sleep, waiting for 10 seconds to allow going into REPL")
pin_power.value = 0

time.sleep(10)
pin_led.value = 0

print(f"Really sleeping now")

pin_sleep_signal.value = 1
time.sleep(1)

time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + 300)
alarm.exit_and_deep_sleep_until_alarms(time_alarm)
