import dbus
import sys
import os

import configparser
from itertools import chain

from urllib.request import Request, urlopen

config = configparser.ConfigParser()

beacons = []

with open(os.getenv('HOME') + '/.verandarc', 'r') as lines:
    lines = chain(("[root]",), lines)
    config.read_file(lines)

    if ('api-key' not in config['root']) or ('ble' not in config['root']):
        sys.exit(0)

    api_key = config['root']['api-key']
    beacon_names = config['root']['ble'].split(" ")

    for beacon_name in beacon_names:
        if ('ble_' + beacon_name + '_address') in config['root']:
            beacon = {'name': beacon_name, 'address': config['root']['ble_' + beacon_name + '_address'], 'last-battery': 0}

            if 'ble_' + beacon_name + '_id' in config['root']:
                beacon['id'] = config['root']['ble_' + beacon_name + '_id']

            if 'ble_' + beacon_name + '_humidity_id' in config['root']:
                beacon['humidity_id'] = config['root']['ble_' + beacon_name + '_humidity_id']

            if 'ble_' + beacon_name + '_signal_id' in config['root']:
                beacon['signal_id'] = config['root']['ble_' + beacon_name + '_signal_id']

            beacons.append(beacon)

from dbus.mainloop.glib import DBusGMainLoop
DBusGMainLoop(set_as_default=True)

bus = dbus.SystemBus()
proxy = bus.get_object("org.bluez", "/org/bluez/hci0")
adapter = dbus.Interface(proxy, "org.bluez.Adapter1")

def signal_received_callback(beacon):
    def signal_received(*args, **kwargs):
        props = args[1]

        if 'ServiceData' in props and '0000fe95-0000-1000-8000-00805f9b34fb' in props['ServiceData']:
            # This is a Xiaomi Mija frame ("LYWSD02" sensor). It gives either temperature,
            # humidity or battery depending on the 11th byte
            data = props['ServiceData']['0000fe95-0000-1000-8000-00805f9b34fb' ]

            data_type = data[12]
            if data_type == 0x04:
                # temperature
                value = ((data[16] << 8) + data[15]) / 10.0
                print (value, 'C')
                headers = {"X-Api-Key": api_key}
                conn = Request('https://veranda.seos.fr/data/sensor/' + beacon['id'] + '?value=' + str(value), headers=headers)
                try:
                    print(urlopen(conn).read())
                except Exception as e:
                    print("HTTP error", e)

            elif data_type == 0x06:
                # humidity
                value = ((data[16] << 8) + data[15]) / 10.0
                print (value, '%')
                headers = {"X-Api-Key": api_key}
                conn = Request('https://veranda.seos.fr/data/sensor/' + beacon['humidity_id'] + '?value=' + str(value), headers=headers)
                try:
                    print(urlopen(conn).read())
                except Exception as e:
                    print("HTTP error", e)

            elif data_type == 0x0a:
                # battery
                print (data)
                value = (data[16] << 8) + data[15]
                print (value, ' battery')
                beacon['last-battery'] = value

            else:
                print ("Unknown data type:", data_type)
                print (data)

        if 'ServiceData' in props and '0000feaa-0000-1000-8000-00805f9b34fb' in props['ServiceData']:
            # This is an Eddystone frame, it gives us the battery voltage in mV
            # this might be useful for the APlant devices that give us a soil humidity
            # value instead of battery in their iBeacon frames
            # AFAIK, they all run on 3V batteries
            data = props['ServiceData']['0000feaa-0000-1000-8000-00805f9b34fb' ]
            voltage_bytes = data[2:4]
            voltage = (voltage_bytes[0] << 8) + voltage_bytes[1]
            beacon['last-battery'] = (voltage/3000) * 100

        if 'ManufacturerData' in props and 0x0085 in props['ManufacturerData'] and 'id' in beacon:
            # This is a SensorBug
            data = props['ManufacturerData'][0x0085]
            battery = int(data[3])
            temperature_bytes = data[-2:]
            temperature = ((temperature_bytes[1] << 8) + temperature_bytes[0]) * 0.0625
            temperature = int.from_bytes(temperature_bytes, byteorder='little', signed=False) * 0.0625
            print(temperature, 'C', battery, '%')
            headers = {"X-Api-Key": api_key}
            conn = Request('https://veranda.seos.fr/data/sensor/' + beacon['id'] + '?value=' + str(temperature) + '\&battery=' + str(battery), headers=headers)
            try:
                print(urlopen(conn).read())
            except Exception as e:
                print("HTTP error", e)

        if 'ManufacturerData' in props and 0x004c in props['ManufacturerData'] and 'id' in beacon:
            # This is an April Brother thingy (generic iBeacon?)
            data = props['ManufacturerData'][0x004c]
            battery = int(data[-3])
            temperature = int(data[-2])
            if temperature > 127:
                temperature = temperature - 0x100

            print(temperature, 'C', battery, '%')
            headers = {"X-Api-Key": api_key}
            conn = Request('https://veranda.seos.fr/data/sensor/' + beacon['id'] + '?value=' + str(temperature) + '\&battery=' + str(battery), headers=headers)
            try:
                print(urlopen(conn).read())
            except Exception as e:
                print("HTTP error", e)

        if 'ManufacturerData' in props and 0x004c in props['ManufacturerData'] and 'humidity_id' in beacon:
            # This is an April Brother thingy (generic iBeacon?)
            # This section is for devices that report humidity instead of battery level
            data = props['ManufacturerData'][0x004c]
            humidity = int(data[-3])
            temperature = int(data[-2])
            if temperature > 127:
                temperature = temperature - 0x100

            print(temperature, 'C', humidity, '%')

            string = 'value=' + str(humidity)
            if beacon['last-battery'] > 0:
                string = string + '\&battery=' + str(beacon['last-battery'])

            headers = {"X-Api-Key": api_key}
            conn = Request('https://veranda.seos.fr/data/sensor/' + beacon['humidity_id'] + '?' + string, headers=headers)
            try:
                print(urlopen(conn).read())
            except Exception as e:
                print("HTTP error", e)

        if 'RSSI' in props and 'signal_id' in beacon:
            signal = int(props['RSSI'])
            print('Signal strenght:', signal, 'dB')
            headers = {"X-Api-Key": api_key}
            conn = Request('https://veranda.seos.fr/data/sensor/' + beacon['signal_id'] + '?value=' + str(signal), headers=headers)
            try:
                print(urlopen(conn).read())
            except Exception as e:
                print("HTTP error", e)

    return signal_received

for beacon in beacons:
    device_path = beacon['address'].replace(":", "_").upper()
    print("Path", device_path, " beacon", beacon)
    bus.add_signal_receiver(signal_received_callback(beacon), path = "/org/bluez/hci0/dev_" + device_path)

    print('Listening to beacon ', beacon)

def power_off():
    properties = dbus.Interface(proxy, "org.freedesktop.DBus.Properties")

    print("Powering off")
    try:
        properties.Set('org.bluez.Adapter1', 'Powered', False)
    except Exception as e:
        print("Could not power off", e)

    check_discovery()

    return 1

def check_discovery():
    properties = dbus.Interface(proxy, "org.freedesktop.DBus.Properties")

    powered = properties.Get('org.bluez.Adapter1', 'Powered')
    if not powered:
        print("Starting power")
        try:
            properties.Set('org.bluez.Adapter1', 'Powered', True)
        except Exception as e:
            print("Could not power on ", e)

    discovering = properties.Get('org.bluez.Adapter1', 'Discovering')

    if not discovering:
        print("Starting discovery")
        try:
            adapter.StartDiscovery()
        except Exception as e:
            print("Could not start discovery ", e)

    return 1

from gi.repository import GLib

power_off()
GLib.timeout_add(5000, check_discovery)

# have to reset things once in a while for some reason
GLib.timeout_add(3600 * 1000 + 10, power_off)

loop = GLib.MainLoop()
loop.run()

