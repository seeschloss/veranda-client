FIRMWARE_VERSION=1
API_KEY="0a77a9542e071d66767e07d99e54738f"
PLACE_ID=16

SENSOR_ID_BATTERY=75
SENSOR_ID_SUPPLY=74
SENSOR_ID_TEMPERATURE=73

import microcontroller
import time
import alarm
import board

import busio
import digitalio
import analogio

import espcamera

from ec200a import *

adc_battery =      analogio.AnalogIn(board.D8)
adc_psu =          analogio.AnalogIn(board.D10)
pin_sleep_signal = digitalio.DigitalInOut(board.D9)
pin_power =        digitalio.DigitalInOut(board.D2)

pin_EC_rx = board.D3
pin_EC_tx = board.D4

voltage_battery = adc_battery.value / 2**16 * 3.3 * 2
voltage_supply = adc_psu.value / 2**16 * 3.3 * 2
esp_temperature = microcontroller.cpu.temperature

print(f"Battery voltage: {voltage_battery} V")
print(f"Supply voltage: {voltage_supply} V")
print(f"Temperature: {esp_temperature} °C")

pin_sleep_signal.switch_to_output()
pin_sleep_signal.value = 0

pin_led = digitalio.DigitalInOut(board.LED)
pin_led.switch_to_output()
pin_led.value = 0

pin_power.switch_to_output()
pin_power.value = 1

uart = busio.UART(rx=pin_EC_rx, tx=pin_EC_tx, baudrate=115200)
http = HTTP_EC200A(uart, [
    ("X-Api-Key", API_KEY),
    ("X-Board-Id", microcontroller.cpu.uid.hex()),
    ("User-Agent", f"Athena/{FIRMWARE_VERSION} (CircuitPython, EC200A-EU)"),
])

#if http.init_modem(baudrate=230400, timeout=25000):
if http.init_modem(baudrate=921600, timeout=25000):
    time.sleep(2)
    network_time = http.network_time()

    print(f"Network time: {network_time}")

    http.send_http_post_json("http://veranda.seos.fr/data/sensor", f'{{ "{SENSOR_ID_BATTERY}": {voltage_battery}, "{SENSOR_ID_SUPPLY}": {voltage_supply}, "{SENSOR_ID_TEMPERATURE}": {esp_temperature} }}')
    time.sleep(1)
    if True or voltage_supply > 2:
        # Pas besoin de photo s'il fait nuit, on va dire
        cam = espcamera.Camera(
            data_pins=board.CAM_DATA,
            external_clock_pin=board.CAM_XCLK,
            pixel_clock_pin=board.CAM_PCLK,
            vsync_pin=board.CAM_VSYNC,
            href_pin=board.CAM_HREF,
            pixel_format=espcamera.PixelFormat.JPEG,
            #frame_size=espcamera.FrameSize.VGA,
            frame_size=espcamera.FrameSize.QSXGA,
            i2c=busio.I2C(board.CAM_SCL, board.CAM_SDA),
            external_clock_frequency=20_000_000,
            framebuffer_count=2,
            grab_mode=espcamera.GrabMode.LATEST)
        cam.hmirror = True
        cam.denoise = 2
        cam.quality = 4
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
#            http.send_sms("33644110300", f"Picture size: {len(img)}, time: {network_time} ({time.localtime(network_time)})")
            try:
                result = http.send_file(f"http://veranda.seos.fr/data/place/{PLACE_ID}/photo", img)
                print(f"Picture sent {result}")
            except Exception as e:
                print(f"Exception while sending picture? {e}")
                pin_led.value = 1
        else:
            print(f"Cannot take picture? {img}")
            pin_led.value = 1

    http.set_uart_speed(115200)
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
