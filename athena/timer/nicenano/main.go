package main

import (
	"machine"
	"time"
	"device/nrf"
	"tinygo.org/x/bluetooth"
	"unsafe"
)

const NRF_SLEEP_TIME = 5 * time.Second
const ESP_SLEEP_TIME = 60 * 30 * time.Second
const ESP_TIMEOUT = 160 * time.Second

// How long the pin must stay continuously low to be considered a valid sleep signal.
const ESP_LOW_DURATION = 3 * time.Second

// One pulse from the ESP encodes this many minutes of requested sleep.
// Must match PULSE_UNIT_MINUTES in the Rust firmware.
const PULSE_UNIT = 5 * time.Minute

// Custom 128-bit UUIDs for the ESP trigger service.
// Use these same UUIDs in nRF Connect to find the characteristic.
var (
	serviceUUID     = parseUUID("AEA00000-1789-0000-0000-000000000000")
	triggerCharUUID = parseUUID("AEA00000-1789-0000-0000-000000000000")
)

func parseUUID(s string) bluetooth.UUID {
	u, err := bluetooth.ParseUUID(s)
	if err != nil {
		panic("bad UUID: " + s)
	}
	return u
}

var (
	adapter = bluetooth.DefaultAdapter

	batteryLevel [2]byte
	supplyLevel  [2]byte
	chargeLevel  [2]byte

	advertisement_options = bluetooth.AdvertisementOptions{
		LocalName:         "ATHENE",
		AdvertisementType: bluetooth.AdvertisingTypeInd, // connectable
		ManufacturerData: []bluetooth.ManufacturerDataElement{
			{CompanyID: 0x1789, Data: batteryLevel[:]},
			{CompanyID: 0x1792, Data: supplyLevel[:]},
			{CompanyID: 0x1794, Data: chargeLevel[:]},
		},
	}

	// Set to true by the BLE write handler; consumed by the main loop.
	triggerESP bool
)

func setupGATT() {
	must("add services", adapter.AddService(
		&bluetooth.Service{
			UUID: serviceUUID,
			Characteristics: []bluetooth.CharacteristicConfig{
				{
					UUID: triggerCharUUID,
					// Write 0x01 to trigger an ESP session immediately.
					Flags: bluetooth.CharacteristicWritePermission |
						bluetooth.CharacteristicWriteWithoutResponsePermission,
					WriteEvent: func(client bluetooth.Connection, offset int, value []byte) {
						if len(value) > 0 && value[0] == 0x01 {
							println("BLE trigger received, scheduling ESP session")
							triggerESP = true
						}
					},
				},
			},
		},
	))
}

func startWatchdog(timeoutSeconds uint32) {
    nrf.WDT.CRV.Set(timeoutSeconds * 32768 - 1)

    // Enable reload register 0
    nrf.WDT.RREN.Set(1)
    // Run during sleep (bit 0), pause during debug halt (bit 3)
    nrf.WDT.CONFIG.Set(1 | (1 << 3))

    nrf.WDT.TASKS_START.Set(1)
}

// decodeSignalPin reads the pulse-encoded sleep duration from the ESP.
//
// The ESP drives the pin through this sequence:
//   1. N short pulses  (LOW for ~400 ms, HIGH for ~400 ms)  ← N = sleep_minutes / PULSE_UNIT
//   2. Sustained LOW                                         ← "done, cut power"
//
// After each falling edge we measure how long the pin stays low:
//   < ESP_LOW_DURATION  → it was a pulse; increment counter; wait for next edge
//   ≥ ESP_LOW_DURATION  → it was the done signal; return the count
//
// Returns (pulseCount, true) on a clean done signal, or (0, false) on timeout.
func decodeSignalPin(pin machine.Pin, timeout time.Duration) (int, bool) {
	fallingEdge := false

	pin.SetInterrupt(machine.PinFalling, func(p machine.Pin) {
		fallingEdge = true
	})
	defer pin.SetInterrupt(machine.PinFalling, nil)

	deadline := time.Now().Add(timeout)
	pulses := 0

	for time.Now().Before(deadline) {
		watchdog_keepalive()

		if !fallingEdge {
			time.Sleep(10 * time.Millisecond)
			continue
		}
		fallingEdge = false

		// Debounce: wait a moment, then confirm the pin is actually low.
		time.Sleep(10 * time.Millisecond)
		if pin.Get() {
			println("Spurious edge, ignoring")
			continue
		}

		// Measure how long the pin stays low.
		lowSince := time.Now()
		isDone := false
		for time.Now().Before(deadline) {
			time.Sleep(5 * time.Millisecond)
			if pin.Get() {
				// Pin recovered — it was a pulse.
				break
			}
			if time.Since(lowSince) >= ESP_LOW_DURATION {
				// Pin has been low long enough — done signal.
				isDone = true
				break
			}
		}

		if isDone {
			println("Done signal received after", pulses, "pulse(s)")
			return pulses, true
		}
		pulses++
		println("Pulse", pulses, "received")

		// Pin is now high again; loop back to wait for the next falling edge.
	}

	println("Timeout waiting for done signal")
	return 0, false
}

func handleESPSession(adv *bluetooth.Advertisement) time.Duration {
	// Stop advertising while the ESP is active.
	adv.Stop()

	// Two pins to handle various boards
	pin_power := machine.P0_08
	pin_power.Configure(machine.PinConfig{Mode: machine.PinOutput})

	pin_power_2 := machine.P0_11
	pin_power_2.Configure(machine.PinConfig{Mode: machine.PinOutput})

	pin_sleep_signal := machine.P0_29
	pin_sleep_signal.Configure(machine.PinConfig{Mode: machine.PinInputPullup})

	pin_power.High()
	pin_power_2.High()

	println("Waiting for ESP to assert signal pin high...")

	// Wait for the ESP to pull the signal pin high (signals boot complete)
	// before we arm the edge interrupt, so we don't catch boot noise.
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) && !pin_sleep_signal.Get() {
		time.Sleep(100 * time.Millisecond)
	}
	println("Signal pin high (or timeout). Decoding sleep duration...")

	pulses, ok := decodeSignalPin(pin_sleep_signal, ESP_TIMEOUT)

	pin_power.Low()
	pin_power_2.Low()
	println("Power off")

	// Resume advertising.
	must("start adv", adv.Start())

	if ok && pulses > 0 {
		duration := time.Duration(pulses) * PULSE_UNIT
		println("Decoded sleep duration:", int(duration.Minutes()), "minute(s)")
		return duration
	}

	if ok && pulses == 0 {
		// Done signal received but no pulses — ESP may be running old firmware
		// that drives the pin low without encoding a duration.  Keep the
		// current default so behaviour is unchanged.
		println("No pulses decoded (old firmware?), using default sleep duration")
	} else {
		println("Timeout, using default sleep duration")
	}
	return ESP_SLEEP_TIME
}

func clearBootloaderCrashFlag() {
    // The UF2 bootloader stores its "double-tap" / crash flag at the start
    // of retained RAM (0x20007F00 on nRF52840 with Adafruit bootloader).
    // Writing 0 tells it "clean boot, reset the counter".
    *(*uint32)(unsafe.Pointer(uintptr(0x20007F00))) = 0

	// Tell the bootloader to skip the double-reset DFU window on the next boot.
    // DFU_DBL_RESET_APP = 0x4ee5677e, stored at DFU_DBL_RESET_MEM = 0x20007F7C
    // Source: adafruit/Adafruit_nRF52_Bootloader linker/nrf52840.ld + src/main.c
    *(*uint32)(unsafe.Pointer(uintptr(0x20007F7C))) = 0x4ee5677e
}

func main() {
	println("start")

	time.Sleep(1 * time.Second)
	clearBootloaderCrashFlag()

	// enable POF, threshold ~2.7 V (V27 = 0b1010)
	nrf.POWER.POFCON.Set((0b1010 << 1) | 1)

	startWatchdog(uint32(ESP_TIMEOUT.Seconds()) + 30);

	pin_3v3 := machine.P0_13
	pin_3v3.Configure(machine.PinConfig{Mode: machine.PinOutput})

	led := machine.LED
	led.Configure(machine.PinConfig{Mode: machine.PinOutput})

	must("enable BLE stack", adapter.Enable())

	// GATT services must be registered before advertising starts.
	setupGATT()

	adv := adapter.DefaultAdvertisement()
	must("config adv", adv.Configure(advertisement_options))
	must("start adv", adv.Start())

	println("advertising...")
	address, _ := adapter.Address()
	println("Go Bluetooth /", address.MAC.String())

	nextESPWakeup := time.Now()
	for {
		if triggerESP || !time.Now().Before(nextESPWakeup) {
			triggerESP = false
			pin_3v3.High()
			led.High()
			sleepDuration := handleESPSession(adv)
			led.Low()
			pin_3v3.Low()
			println("Next ESP wakeup in", int(sleepDuration.Minutes()), "minute(s)")
			nextESPWakeup = time.Now().Add(sleepDuration)
		}

		watchdog_keepalive()

		must("config adv", adv.Configure(advertisement_options))
		must("start adv", adv.Start())
		time.Sleep(NRF_SLEEP_TIME)
		adv.Stop()
	}
}

func watchdog_keepalive() {
	// magic reload value, cf. https://docs.nordicsemi.com/bundle/ps_nrf5340/page/wdt.html
	nrf.WDT.RR[0].Set(0x6E524635)
}

func must(action string, err error) {
	if err != nil {
		println("failed to " + action + ": " + err.Error())
	}
}
