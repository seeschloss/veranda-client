import digitalio
import analogio

import alarm
import time
import board

TIMEOUT_SECONDS=160

pin_power =        digitalio.DigitalInOut(board.D0)
pin_sleep_signal = digitalio.DigitalInOut(board.D2)
adc_battery =      analogio.AnalogIn(board.A5)
adc_psu =          analogio.AnalogIn(board.A1)

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

voltage_battery = adc_battery.value / 2**16 * 3.3 * 2
voltage_supply = adc_psu.value / 2**16 * 3.3 * 2

print(f"Battery voltage: {voltage_battery} V")
print(f"Supply voltage: {voltage_supply} V")

if False and voltage_battery < 3.4:
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

print("Sleeping for good")
time_alarm = alarm.time.TimeAlarm(monotonic_time=time.monotonic() + 60 * 20 - TIMEOUT_SECONDS)
alarm.exit_and_deep_sleep_until_alarms(time_alarm)
# c'est du deep sleep, ça s'arrête là
