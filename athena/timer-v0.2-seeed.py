import digitalio
import analogio
import microcontroller

import alarm
import time
import board

import _bleio
import struct

import time

def broadcast_values(values, timeout = 10, name="ATHENA"):
    print("Advertising through BLE")
    advertisement = struct.pack("<" "BB" "B"
                                    "BB" "6s"
                                    "BB" "BB" "B",
        2, 1, 6, # Flags / LE General Discoverable && BR/EDR Not Supported
        len("ATHENA") + 1, 9, "ATHENA", # Complete local name
        3 + (len(values) * (1 + 4)), 0xFF, 0x89, 0x17, # Manufacturer data / Mf ID 0x17 0x89
            len(values)
    )

    print(values)
    for sensor_id, sensor_value in values.items():
        advertisement += struct.pack("<Bf", sensor_id, sensor_value)

    _bleio.adapter.start_advertising(advertisement, connectable=False, timeout=timeout)

def broadcast_values_text(values, timeout = 10, name="ATHENA"):
    print("Advertising through BLE")
    data_string = f"{values}"
    advertisement = struct.pack("<" "BB" "B"
                                    "BB" "6s"
                                    "BB" "BB" f"{len(data_string)}s",
        2, 1, 6, # Flags / LE General Discoverable && BR/EDR Not Supported
        len("ATHENA") + 1, 9, "ATHENA", # Complete local name
        3 + len(data_string), 0xFF, 0x89, 0x17, # Manufacturer data / Mf ID 0x17 0x89
            data_string
    )

    print(values)

    _bleio.adapter.start_advertising(advertisement, connectable=False, timeout=timeout)

TIMEOUT_SECONDS = 160
ESP_SLEEP_TIME = 60 * 20
NRF_SLEEP_TIME = 10

TIME_TO_WAKEUP = struct.unpack("<I", alarm.sleep_memory[0:4])[0]

if microcontroller.cpu.reset_reason != microcontroller.ResetReason.DEEP_SLEEP_ALARM:
    print(f"Wake up reason: {microcontroller.cpu.reset_reason}")
    TIME_TO_WAKEUP = 0

SENSOR_ID_BATTERY_VOLTAGE=69
SENSOR_ID_SUPPLY_VOLTAGE=70
SENSOR_ID_BATTERY_CURRENT=68
SENSOR_ID_SUPPLY_CURRENT=71
SENSOR_ID_BOARD_CURRENT=72
SENSOR_ID_TEMPERATURE=59

charge_rate = digitalio.DigitalInOut(board.CHARGE_RATE)
charge_rate.switch_to_output()
charge_rate.value = False

pin_power =        digitalio.DigitalInOut(board.D0)
pin_sleep_signal = digitalio.DigitalInOut(board.D2)
adc_battery =      analogio.AnalogIn(board.A5)
adc_psu =          analogio.AnalogIn(board.A1)

vbat_enable = digitalio.DigitalInOut(board.READ_BATT_ENABLE)
vbat_enable.switch_to_output()
vbat_enable = 0
adc_battery_2 =    analogio.AnalogIn(board.VBATT)
voltage_battery_2 = adc_battery_2.value * 3.3 / 2**16 * 1510/510
# d'où vient cette formule, c'est pas clair, je l'ai reprise de :
# https://forum.seeedstudio.com/t/xiao-nrf52840-battery-voltage-not-readable-on-platformio/268637
vbat_enable = 1

voltage_battery   = adc_battery.value / 2**16 * 3.3 * 2
voltage_supply    = adc_psu.value / 2**16 * 3.3 * 2

adc_battery.deinit()
adc_psu.deinit()

adc_battery = digitalio.DigitalInOut(board.D5)
adc_battery.switch_to_input(pull = digitalio.Pull.DOWN)

adc_psu = digitalio.DigitalInOut(board.D1)
adc_psu.switch_to_input(pull = digitalio.Pull.DOWN)

print(f"Battery voltage: {voltage_battery} V")
print(f"Battery voltage 2: {voltage_battery_2} V")
print(f"Supply voltage: {voltage_supply} V")

if (TIME_TO_WAKEUP > 0):
    alarm.sleep_memory[0:4] = struct.pack("<I", TIME_TO_WAKEUP - NRF_SLEEP_TIME)

    # On se réveille régulièrement pour envoyer l'état du truc
    broadcast_values_text({SENSOR_ID_SUPPLY_VOLTAGE: voltage_supply,
                      SENSOR_ID_SUPPLY_CURRENT: 0,
                      SENSOR_ID_BATTERY_VOLTAGE: voltage_battery_2,
                      SENSOR_ID_BATTERY_CURRENT: 0,
                      SENSOR_ID_BOARD_CURRENT: 0,
                      SENSOR_ID_TEMPERATURE: microcontroller.cpu.temperature,
                      "reason": microcontroller.cpu.reset_reason,
                      "ttl": TIME_TO_WAKEUP,
                      },
                     timeout=10, name="ATHENA")

    print(f"It is not yet time to run the ESP: time to wakeup is {TIME_TO_WAKEUP}s, sleeping for {NRF_SLEEP_TIME}s")
    time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + NRF_SLEEP_TIME)
    alarm.exit_and_deep_sleep_until_alarms(time_alarm)
    # Deep sleep, fin de l'exécution

pin_alarm = alarm.pin.PinAlarm(pin=board.D4, value=True, pull=True)

led_R = digitalio.DigitalInOut(board.LED_RED)
led_R.switch_to_output()
led_R.value = 1
led_G = digitalio.DigitalInOut(board.LED_GREEN)
led_G.switch_to_output()
led_G.value = 1
led_B = digitalio.DigitalInOut(board.LED_BLUE)
led_B.switch_to_output()
led_B.value = 1

led_G.value = 0
led_B.value = 0

pin_power.switch_to_output()
pin_power.value = 0

pin_sleep_signal.switch_to_input(pull = digitalio.Pull.DOWN)

if False and voltage_battery_2 < 3.4:
    # Pas assez pour faire tourner le modem.
    # On verra dans 5 minutes si ça va mieux, ça coûte pas grand chose
    # 10 secondes pour pouvoir se connecter au REPL micropython
    time.sleep(10)
    time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + 300)
    alarm.exit_and_deep_sleep_until_alarms(time_alarm)
    # c'est du deep sleep, ça s'arrête là

if False and voltage_supply < 2:
    # Pas de soleil, probablement
    # On verra dans 5 minutes si ça va mieux, ça coûte pas grand chose
    # 10 secondes pour pouvoir se connecter au REPL micropython
    time.sleep(10)
    time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + 300)
    alarm.exit_and_deep_sleep_until_alarms(time_alarm)
    # c'est du deep sleep, ça s'arrête là

pin_power.value = 1

broadcast_values_text({
                  "reason": microcontroller.cpu.reset_reason,
                  "ttl": TIME_TO_WAKEUP,
                  },
                 timeout=10, name="ATHENA")

print("Sleeping to let Athena do her job")
# 10 secondes pour pouvoir se connecter au REPL micropython
time.sleep(10)
led_B.value = 1

print("Waiting for a signal or timeout")
# D'après mes tests, 120 secondes suffisent (normalement ça prend ~75 secondes)
time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + TIMEOUT_SECONDS - 10 - 5)
wake_alarm = alarm.light_sleep_until_alarms(time_alarm, pin_alarm)

if wake_alarm == pin_alarm:
    print("Athena told us it's finished, turning power off")
else:
    print("Athena didn't finish within 2 minutes, turning power off")

pin_power.value = 0

led_G.value = 1

alarm.sleep_memory[0:4] = struct.pack("<I", ESP_SLEEP_TIME)

print("Sleeping for good")
time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + NRF_SLEEP_TIME)
alarm.exit_and_deep_sleep_until_alarms(time_alarm)
# c'est du deep sleep, ça s'arrête là
