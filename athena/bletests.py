import _bleio
import time
import struct

def scan():
    results = _bleio.adapter.start_scan(timeout=15, interval=0.005, window=0.005, extended=True, buffer_size=1024 * 50, minimum_rssi=-100, active=False)

    found = set()

    for result in results:
        if result.address not in found and result.advertisement_bytes.find(b'ATHE') >= 0:
            found.add(result.address)
            print(result.address)
            print(result.advertisement_bytes)

    _bleio.adapter.stop_scan()
    print("")

def broadcast_values(values, timeout = 10, name="ATHENE"):
    print("Advertising through BLE")
    advertisement = struct.pack("<" "BB" "B"
                                    "BB" "6s"
                                    "BB" "BB" "B",
        2, 1, 6, # Flags / LE General Discoverable && BR/EDR Not Supported
        len(name) + 1, 9, name, # Complete local name
        3 + (len(values) * (1 + 2)), 0xFF, 0x89, 0x17, # Manufacturer data / Mf ID 0x17 0x89
            len(values)
    )

    print(values)
    for sensor_id, sensor_value in values.items():
        advertisement += struct.pack("<Be", sensor_id, sensor_value)

    _bleio.adapter.start_advertising(advertisement, connectable=False, timeout=timeout)

def broadcast_values_text(values, timeout = 10, name="ATHENE"):
   print("Advertising through BLE")
   data_string = f"{values}"
   advertisement = struct.pack("<" "BB" "B"
                                   "BB" "6s"
                                   "BB" "BB" f"{len(data_string)}s",
       2, 1, 6, # Flags / LE General Discoverable && BR/EDR Not Supported
       len(name) + 1, 9, name, # Complete local name
       3 + len(data_string), 0xFF, 0x89, 0x17, # Manufacturer data / Mf ID 0x17 0x89
           data_string
   )

   print(values)

   _bleio.adapter.start_advertising(advertisement, connectable=False, timeout=timeout)

broadcast_values({0: 1, 1: 42.67})
#broadcast_values_text('x' * 16)
time.sleep(30)

#while True:
#    scan()
#    time.sleep(1)
