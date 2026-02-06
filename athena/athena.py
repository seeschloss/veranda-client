FIRMWARE_VERSION=1
ATHENA_URL="http://athena.seos.fr"

import microcontroller
import time
import alarm
import board

import busio
import digitalio
import analogio
import sdcardio

import json

from ble import BLE

from ov5640 import Cam

from ec200a import *
from dcim import DCIM

adc_battery =      analogio.AnalogIn(board.D8)
adc_psu =          analogio.AnalogIn(board.D10)
pin_power =        digitalio.DigitalInOut(board.D2)

pin_EC_rx = board.D3
pin_EC_tx = board.D4

time_start = time.monotonic()

voltage_battery = adc_battery.value / 2**16 * 3.3 * 2
voltage_supply = adc_psu.value / 2**16 * 3.3 * 2
esp_temperature = microcontroller.cpu.temperature

adc_battery.deinit()
adc_psu.deinit()

print(f"Battery voltage: {voltage_battery} V")
print(f"Supply voltage: {voltage_supply} V")
print(f"Temperature: {esp_temperature} °C")

ble = BLE("ATHENA")
data = ble.receive(timeout=2)
if data:
    print(f"Received BLE data: {data}")
    voltage_psu = data[0]
    voltage_battery = data[1]

print(f"Battery voltage: {voltage_battery} V")
print(f"Supply voltage: {voltage_supply} V")
print(f"Temperature: {esp_temperature} °C")

pin_led = digitalio.DigitalInOut(board.LED)
pin_led.switch_to_output()
pin_led.value = 0

pin_power.switch_to_output()
pin_power.value = 1

img = None
img_quality = 0

try:
    camera = Cam()
    img = camera.take()
    img_quality = camera.cam.quality

except Exception as e:
    print(f"Cannot take picture: {e}")
    img = None

network_time = 0
uart = busio.UART(rx=pin_EC_rx, tx=pin_EC_tx, baudrate=115200)
http = HTTP_EC200A(uart, [
    ("X-Board-Id", microcontroller.cpu.uid.hex()),
    ("User-Agent", f"Athena/{FIRMWARE_VERSION} (CircuitPython, EC200A-EU)"),
])
if http.init_modem(baudrate=921600, timeout=25000):
    time.sleep(2)
    network_time = http.network_time()

    print(f"Network time: {network_time}")


if img is not None:
    print(f"Picture size: {len(img)}")

    sd = None
    pin_led.deinit()
    try:
        sd = sdcardio.SDCard(board.SPI(), board.SDCS)
        dcim = DCIM(sd)

        if network_time > 0:
            t = time.localtime(network_time)
            timestamp = "{:04d}{:02d}{:02d}{:02d}{:02d}".format(*t[:5])
        else:
            timestamp = ""

        (result, filename) = dcim.store(img, ".JPG", "." + timestamp)
        if result:
            print(f"Image saved as '{filename}'")
        else:
            print(f"Image could not be saved, for some reason: {filename}")
    except Exception as e:
        print(f"Probably no SD card present: {e}")
    finally:
        if sd is not None:
            sd.deinit()
        board.SPI().deinit()

    pin_led = digitalio.DigitalInOut(board.LED)
    pin_led.switch_to_output()
    pin_led.value = 0
else:
    print(f"Cannot take picture? {img}")
    pin_led.value = 1

pin_sleep_signal = digitalio.DigitalInOut(board.D9)
pin_sleep_signal.switch_to_output()
pin_sleep_signal.value = 0

try:
    if http.network_registration():
        img_size = 0
        if img is not None:
            if dcim and filename:
                print(f"Adding timestamp ({timestamp}) to file ({filename})")
                dcim.add_timestamp(filename, timestamp)
            img_size = len(img)

        http.send_http_post_json(f"{ATHENA_URL}/data/sensor", json.dumps({
            "battery": {"value": voltage_battery, "type": "voltage"},
            "supply": {"value": voltage_supply, "type": "voltage"},
            "temperature": {"value": esp_temperature, "type": "temperature"},
            "image_size": {"value": img_size, "name": "Image size"},
            "image_quality": {"value": img_quality, "name": "Image quality"},
        }))

        time.sleep(1)

        time_transfer_start = time.monotonic()

        if img is not None:
            result = http.send_file(f"{ATHENA_URL}/data/photo", img)
            print(f"Picture sent {result}")


        time_stop = time.monotonic()

        http.set_uart_speed(115200)

        http.send_http_post_json(f"{ATHENA_URL}/data/sensor", json.dumps({
            "duration_total": {"value": time_stop - time_start, "type": "duration", "name": "Total duration"},
            "duration_transfer": {"value": time_stop - time_transfer_start, "type": "duration", "name": "Transfer duration"},
        }))

        time.sleep(1)

        http.modem_shutdown()
        time.sleep(10)
    else:
        print(f"Couldn't init modem")
        http.set_uart_speed(115200)
        http.modem_shutdown()
except Exception as e:
    print(f"Exception while sending picture? {e}")
    pin_led.value = 1

print(f"Time to sleep, waiting for 10 seconds to allow going into REPL")
pin_power.value = 0

time.sleep(10)
pin_led.value = 0

print(f"Really sleeping now")

pin_sleep_signal.value = 1
time.sleep(1)

time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + 300)
alarm.exit_and_deep_sleep_until_alarms(time_alarm)
