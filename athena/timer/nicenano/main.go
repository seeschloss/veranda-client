package main

import (
	"machine"
	"time"

	"tinygo.org/x/bluetooth"
)

const NRF_SLEEP_TIME = 5 * time.Second
const ESP_SLEEP_TIME = 60 * 30 * time.Second
const ESP_TIMEOUT = 160 * time.Second

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

func waitForInterrupt(pin machine.Pin, lowDuration time.Duration, timeout time.Duration) bool {
	fallingEdge := false

	pin.SetInterrupt(machine.PinFalling, func(p machine.Pin) {
		println("Falling edge")
		fallingEdge = true
	})

	deadline := time.Now().Add(timeout)

	for time.Now().Before(deadline) {
		println("Falling edge?", fallingEdge)
		if fallingEdge {
			fallingEdge = false
			time.Sleep(10 * time.Millisecond)

			if !pin.Get() {
				start := time.Now()
				valid := true

				for time.Since(start) < lowDuration {
					if pin.Get() {
						valid = false
						println("Pin went high again")
						break
					}
					time.Sleep(10 * time.Millisecond)
				}

				if valid {
					// Pin stayed low for entire duration
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
	pin_sleep_signal.Configure(machine.PinConfig{Mode: machine.PinInput})

	pin_power.High()
	println("Sleeping 5 seconds to give the ESP time before setting up signal interrupt")
	time.Sleep(5 * time.Second)

	println("Waiting")
	ok := waitForInterrupt(pin_sleep_signal, time.Second * 1, ESP_TIMEOUT)
	if ok {
		println("Success: Pin stayed low for 1 second")
	} else {
		println("Timeout: Condition not met")
	}

	println("Timeout has passed, removing interrupt and turning power off")
	pin_sleep_signal.SetInterrupt(machine.PinFalling, nil)
	pin_power.Low()

	// Resume advertising.
	must("start adv", adv.Start())
}

func main() {
	println("start")

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
