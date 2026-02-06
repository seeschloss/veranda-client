package main

import (
	"time"
	"machine"
	"encoding/binary"

	"tinygo.org/x/bluetooth"
)

const NRF_SLEEP_TIME = 30 * time.Second
//const ESP_SLEEP_TIME = 2 * 30 * time.Second
const ESP_SLEEP_TIME = 60 * 30 * time.Second
const ESP_TIMEOUT = 160 * time.Second

var (
	adapter = bluetooth.DefaultAdapter
	battery bluetooth.Characteristic

	batteryLevel [2]byte
	supplyLevel [2]byte

	advertisement_options = bluetooth.AdvertisementOptions{
		LocalName: "ATHENE",
		AdvertisementType: bluetooth.AdvertisingTypeNonConnInd,
		ManufacturerData: []bluetooth.ManufacturerDataElement{
			{CompanyID: 0x1789, Data: batteryLevel[:]},
			{CompanyID: 0x1792, Data: supplyLevel[:]},
		},
	}
)

type device struct {
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

		// Up to 5 seconds delay before reacting is fine
        time.Sleep(5 * time.Second)
    }

    // Timeout
    return false
}
func handleESPSession() bool {
	pin_power := machine.D0
	pin_power.Configure(machine.PinConfig{Mode: machine.PinOutput})

	pin_sleep_signal := machine.D2
	pin_sleep_signal.Configure(machine.PinConfig{Mode: machine.PinInputPullup})

	pin_power.High()
	println("Sleeping 5 seconds to give time for the ESP before setting up signal interrupt")
	time.Sleep(5 * time.Second)

	println("Waiting")
	ok := waitForInterrupt(pin_sleep_signal, time.Second * 1, time.Second  *120)
	if ok {
		println("Success: Pin stayed low for 1 second")
	} else {
		println("Timeout: Condition not met")
	}

	println("Timeout has passed, removing interrupt and turning power off")
	pin_sleep_signal.SetInterrupt(machine.PinFalling, nil)
	pin_power.Low()

	return true
}

func main() {
	println("start")
	led := machine.LED
	led.Configure(machine.PinConfig{Mode: machine.PinOutput})
	led.Low()
	time.Sleep(time.Second * 5)
	led.High()
	machine.InitADC()
	ADCBattery := machine.ADC{machine.P0_31}
	ADCBattery.Configure(machine.ADCConfig{})

	ADCSupply := machine.ADC{machine.A1}
	ADCSupply.Configure(machine.ADCConfig{})

	voltage_battery := uint16(float32(ADCBattery.Get()) / 65535 * 4.2 * 1000)
	voltage_supply := uint16(float32(ADCSupply.Get()) / 65535 * 4.2 * 1000)
	println("Battery: ", voltage_battery, " mV")
	println("PSU: ", voltage_supply, " mV")
	// 2^16 / 3.3 / 2

	must("enable BLE stack", adapter.Enable())
	adv := adapter.DefaultAdvertisement()
	must("config adv", adv.Configure(advertisement_options))
	must("start adv", adv.Start())

	println("advertising...")
	address, _ := adapter.Address()
	println("Go Bluetooth /", address.MAC.String())

	for {
		handleESPSession()

		// Then wait until next session
		nextESPWakeup := time.Now().Add(ESP_SLEEP_TIME)
		for (nextESPWakeup.Compare(time.Now()) > 0) {
			voltage_battery = uint16(float32(ADCBattery.Get()) / 65535 * 4.2 * 1000)
			voltage_supply = uint16(float32(ADCSupply.Get()) / 65535 * 4.2 * 1000)
			binary.LittleEndian.PutUint16(batteryLevel[:], voltage_battery)
			binary.LittleEndian.PutUint16(supplyLevel[:], voltage_supply)

			println("Battery: ", voltage_battery, " mV")
			println("PSU: ", voltage_supply, " mV")

			println("Sleeping for", NRF_SLEEP_TIME)
			adv.Configure(advertisement_options)
			adv.Start()
			time.Sleep(NRF_SLEEP_TIME)
			adv.Stop()
		}
	}
}

func must(action string, err error) {
	if err != nil {
		println("failed to " + action + ": " + err.Error())
	}
}
