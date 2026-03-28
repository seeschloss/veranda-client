package main

import (
	"encoding/binary"
	"machine"
	"time"
	"device/nrf"
)

const ESP_SLEEP_TIME = 60 * 30 * time.Second
const ESP_TIMEOUT = 160 * time.Second

// I2C target configuration.
//
// Without the SoftDevice active, TinyGo's TWIS peripheral works reliably.
// The SoftDevice lives in flash but is never enabled: removing the
// tinygo.org/x/bluetooth import means sd_softdevice_enable() is never
// called, so the SoftDevice never intercepts TWIS events.
//
// Build and flash exactly as before:
//   tinygo build -target=nicenano -o timer.uf2
//
// Protocol (4 bytes written by the ESP32 master):
//   byte 0 : 0x17  (magic)
//   byte 1 : 0x89  (magic)
//   byte 2 : duration high byte  (uint16 big-endian, seconds)
//   byte 3 : duration low byte
const (
	i2cTargetAddr = uint8(0x42) // must match NRF_I2C_ADDR on the ESP side
	i2cMagic0     = uint8(0x17)
	i2cMagic1     = uint8(0x89)
	i2cMsgLen     = 4
)

// ---------------------------------------------------------------------------
// Hardware
// ---------------------------------------------------------------------------

func startWatchdog(timeoutSeconds uint32) {
	nrf.WDT.CRV.Set(timeoutSeconds*32768 - 1)
	nrf.WDT.RREN.Set(1)
	nrf.WDT.CONFIG.Set(1 | (1 << 3)) // run during sleep, pause at debug halt
	nrf.WDT.TASKS_START.Set(1)
}

// ---------------------------------------------------------------------------
// I2C target receive
// ---------------------------------------------------------------------------

// listenForESPMessage configures I2C0 as a TWIS target and blocks until it
// receives a valid 4-byte message from the ESP, or until the timeout fires.
//
// This works because the SoftDevice is never enabled (no bluetooth import),
// so TWIS events are delivered directly to the application as on any bare-
// metal nRF52840 target.
func listenForESPMessage(timeout time.Duration) (time.Duration, bool) {
	err := machine.I2C0.Configure(machine.I2CConfig{
        Frequency: 400_000,
		SCL:     machine.P0_17,
		SDA:     machine.P0_20,
		Mode:    machine.I2CModeTarget,
	})
	if err != nil {
		println("I2C configure failed:", err.Error())
		return 0, false
	}

    err = machine.I2C0.Listen(uint8(i2cTargetAddr))
	if err != nil {
		println("I2C listen failed:", err.Error())
		return 0, false
	}

	buf := make([]byte, i2cMsgLen)

	// WaitForEvent is the nRF52840's event-driven I2C target API in TinyGo.
	// It blocks until the master completes a write transaction to our address.
	// If your TinyGo version exposes Listen() instead, replace these two calls
	// with: n, err := machine.I2C0.Listen(buf)
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		event, count, err := machine.I2C0.WaitForEvent(buf)
		if err != nil {
			println("I2C WaitForEvent error:", err.Error())
			return 0, false
		}

		// I2CReceive means the master finished writing to us.
		if event != machine.I2CReceive {
			println("I2C event:", event)
			continue
		}
		if count < i2cMsgLen {
			println("I2C message too short:", count)
			continue
		}
		if buf[0] != i2cMagic0 || buf[1] != i2cMagic1 {
			println("I2C bad magic:", buf[0], buf[1])
			continue
		}

		seconds := binary.BigEndian.Uint16(buf[2:4])
		println("I2C received sleep duration:", seconds, "seconds")
		return time.Duration(seconds) * time.Second, true
	}

	println("I2C timeout")
	return 0, false
}

// ---------------------------------------------------------------------------
// ESP session
// ---------------------------------------------------------------------------

// handleESPSession powers on the ESP, waits for it to send the sleep duration
// over I2C, then powers it off. Returns the decoded duration or ESP_SLEEP_TIME.
func handleESPSession() time.Duration {
	pin_power := machine.P0_08
	pin_power.Configure(machine.PinConfig{Mode: machine.PinOutput})

	pin_power_2 := machine.P0_11
	pin_power_2.Configure(machine.PinConfig{Mode: machine.PinOutput})

	// Arm I2C target BEFORE powering on the ESP so no message can be missed.
	// (The Configure call sets up the TWIS peripheral but does not block yet;
	// WaitForEvent/Listen below is the blocking call.)
	println("Arming I2C target, powering on ESP...")
	pin_power.High()
	pin_power_2.High()

	sleepDuration, ok := listenForESPMessage(ESP_TIMEOUT)

	pin_power.Low()
	pin_power_2.Low()
	println("ESP powered off")

	if ok {
		return sleepDuration
	}
	println("No valid I2C message, using default sleep duration")
	return ESP_SLEEP_TIME
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

func main() {
    time.Sleep(5 * time.Second)
	println("start")

	startWatchdog(uint32(ESP_TIMEOUT.Seconds()) + 30)

	pin_3v3 := machine.P0_13
	pin_3v3.Configure(machine.PinConfig{Mode: machine.PinOutput})

	led := machine.LED
	led.Configure(machine.PinConfig{Mode: machine.PinOutput})

	nextESPWakeup := time.Now() // trigger immediately on first boot
	for {
		if !time.Now().Before(nextESPWakeup) {
			pin_3v3.High()
			led.High()
			sleepDuration := handleESPSession()
			led.Low()
			pin_3v3.Low()
			println("Next ESP wakeup in", int(sleepDuration.Minutes()), "minute(s)")
			nextESPWakeup = time.Now().Add(sleepDuration)
		}

		// Pet the watchdog.
		nrf.WDT.RR[0].Set(0x6E524635)

		time.Sleep(5 * time.Second)
	}
}
