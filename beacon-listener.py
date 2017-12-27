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
        os.exit(0)

    api_key = config['root']['api-key']
    beacon_names = config['root']['ble'].split(" ")

    for beacon_name in beacon_names:
        if ('ble_' + beacon_name + '_address') in config['root']:
            beacon = {'name': beacon_name, 'address': config['root']['ble_' + beacon_name + '_address']}

            if 'ble_' + beacon_name + '_id' in config['root']:
                beacon['id'] = config['root']['ble_' + beacon_name + '_id']

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

