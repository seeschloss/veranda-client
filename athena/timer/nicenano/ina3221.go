package main

import (
	"encoding/binary"
	"machine"
)

// INA3221 I2C address (A0 pin → GND=0x40, VS=0x41, SDA=0x42, SCL=0x43)
const INA3221_ADDR = 0x40

// INA3221 bus voltage registers (one per channel)
const (
	INA3221_REG_BUS_CH1 = 0x02
	INA3221_REG_BUS_CH2 = 0x04
	INA3221_REG_BUS_CH3 = 0x06
)

// INA3221_BUS_VOLTAGE_LSB_MV is 8 mV per LSB.
// The raw register value is left-aligned in 13 bits; shift right by 3.
const INA3221_BUS_VOLTAGE_LSB_MV = 8

// ina3221Init configures I2C0 on the INA pins and brings the chip up.
// Call this before any reads, and after an ESP session ends.
func ina3221Init() {
	machine.P0_17.Configure(machine.PinConfig{Mode: machine.PinInputPullup})
    machine.P0_20.Configure(machine.PinConfig{Mode: machine.PinInputPullup})

	err := machine.I2C0.Configure(machine.I2CConfig{
		SCL:       machine.P0_17,
		SDA:       machine.P0_20,
		Frequency: machine.TWI_FREQ_100KHZ,
	})
	if err != nil {
		println("I2C init failed:", err.Error())
	}
}

// ina3221Release reconfigures the I2C pins as output-low so they don't
// interfere with the ESP's access to the same I2C bus.
func ina3221Release() {
	pin_scl := machine.P0_17
	pin_sda := machine.P0_20
	pin_scl.Configure(machine.PinConfig{Mode: machine.PinOutput})
	pin_sda.Configure(machine.PinConfig{Mode: machine.PinOutput})
	pin_scl.Low()
	pin_sda.Low()
}

// ina3221ReadBusVoltage reads a bus voltage register and returns millivolts.
func ina3221ReadBusVoltage(reg uint8) (uint16, error) {
	var buf [2]byte
	err := machine.I2C0.Tx(INA3221_ADDR, []byte{reg}, buf[:])
	if err != nil {
		return 0, err
	}
	// Raw value is big-endian; bus voltage is bits [15:3], LSB = 8 mV
	raw := int16(binary.BigEndian.Uint16(buf[:]))
	mv := uint16((raw >> 3) * INA3221_BUS_VOLTAGE_LSB_MV)
	return mv, nil
}

// readVoltages reads channel 1 (battery) and channel 2 (supply) in mV.
func readVoltages() (v1 uint16, v2 uint16, v3 uint16) {
	var err error
	v1, err = ina3221ReadBusVoltage(INA3221_REG_BUS_CH1)
	if err != nil {
		println("INA3221 ch1 read error:", err.Error())
	}
	v2, err = ina3221ReadBusVoltage(INA3221_REG_BUS_CH2)
	if err != nil {
		println("INA3221 ch2 read error:", err.Error())
	}
	v3, err = ina3221ReadBusVoltage(INA3221_REG_BUS_CH3)
	if err != nil {
		println("INA3221 ch3 read error:", err.Error())
	}
	return
}
