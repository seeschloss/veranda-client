package main

import (
	"machine"
	"time"
	"device/nrf"
	"tinygo.org/x/bluetooth"
)

const NRF_SLEEP_TIME = 5 * time.Second
const ESP_SLEEP_TIME = 60 * 30 * time.Second
const ESP_TIMEOUT = 160 * time.Second

// How long the pin must stay continuously low to be considered a valid sleep signal.
const ESP_LOW_DURATION = 3 * time.Second

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

func waitForInterrupt(pin machine.Pin, lowDuration time.Duration, timeout time.Duration) bool {
	fallingEdge := false

	pin.SetInterrupt(machine.PinFalling, func(p machine.Pin) {
		println("Falling edge detected")
		fallingEdge = true
	})

	deadline := time.Now().Add(timeout)

	for time.Now().Before(deadline) {
		if fallingEdge {
			fallingEdge = false
			time.Sleep(10 * time.Millisecond)

			if !pin.Get() {
				start := time.Now()
				valid := true

				for time.Since(start) < lowDuration {
					if pin.Get() {
						valid = false
						println("Pin went high again, ignoring edge")
						break
					}
					time.Sleep(10 * time.Millisecond)
				}

				if valid {
					// Pin stayed low for the full debounce duration
					return true
				}
			}
		}

		time.Sleep(100 * time.Millisecond)
	}

	// Timeout
	return false
}

func handleESPSession(adv *bluetooth.Advertisement) {
	// Stop advertising while the ESP owns the I2C bus.
	adv.Stop()

	pin_power := machine.P0_08
	pin_power.Configure(machine.PinConfig{Mode: machine.PinOutput})

	pin_sleep_signal := machine.P0_29
	pin_sleep_signal.Configure(machine.PinConfig{Mode: machine.PinInputPullup})

	pin_power.High()

	// Wait for the ESP to assert the signal pin high before arming the
	// interrupt. This prevents spurious falling-edge triggers from the pin
	// floating or bouncing low during ESP boot.
	println("Waiting for ESP to assert signal pin high...")
	for !pin_sleep_signal.Get() {
		time.Sleep(100 * time.Millisecond)
	}
	println("Signal pin is high, ESP has started. Watching for falling edge...")

	ok := waitForInterrupt(pin_sleep_signal, ESP_LOW_DURATION, ESP_TIMEOUT)
	if ok {
		println("Success: pin stayed low for required duration, ESP is done")
	} else {
		println("Timeout: ESP did not signal completion within", ESP_TIMEOUT)
	}

	pin_sleep_signal.SetInterrupt(machine.PinFalling, nil)
	pin_power.Low()
	println("Power off")

	// Resume advertising.
	must("start adv", adv.Start())
}

func main() {
	println("start")

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
			handleESPSession(adv)
			led.Low()
			pin_3v3.Low()
			nextESPWakeup = time.Now().Add(ESP_SLEEP_TIME)
		}

		// magic reload value, cf. https://docs.nordicsemi.com/bundle/ps_nrf5340/page/wdt.html
		nrf.WDT.RR[0].Set(0x6E524635)

		must("config adv", adv.Configure(advertisement_options))
		must("start adv", adv.Start())
		time.Sleep(NRF_SLEEP_TIME)
		adv.Stop()
	}
}

func must(action string, err error) {
	if err != nil {
		println("failed to " + action + ": " + err.Error())
	}
}
