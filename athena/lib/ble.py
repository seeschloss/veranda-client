import _bleio
import struct

class BLE:
    name = "ATHENA"

    def __init__(self, name):
        self.name = name

    def receive(self, timeout = 10):
        prefix = struct.pack("<" "B" "BBB", 3, 0xFF, 0x89, 0x17)

        results = _bleio.adapter.start_scan(prefixes = prefix, timeout=15, extended=True, buffer_size=1024, active=False)

        found = set()

        for result in results:
            prefix_length = 5 + len(self.name)
            found.add(result.address)
            manufacturer_id = result.advertisement_bytes[prefix_length + 2:prefix_length + 4]
            payload = result.advertisement_bytes[prefix_length + 4:]

            data_length = payload[0]

            data = {}
            try:
                raw_data = struct.unpack("<" + ("Be" * data_length), payload[1:])

                # convertir une liste en dict avec entrées impaires comme clés et entrées paires comme valeurs
                it = iter(raw_data)
                data = dict(zip(it, it))
            except Exception as e:
                print(f"Cannot decode packet? {e}")
                print(f"Payload was: {payload}")

            _bleio.adapter.stop_scan()

            return data

        _bleio.adapter.stop_scan()

    def broadcast_values(self, values, timeout = 10):
        print("Advertising through BLE")
        advertisement = struct.pack("<" "BB" "B"
                                        "BB" f"{len(self.name)}s"
                                        "BB" "BB" "B",
            2, 1, 6, # Flags / LE General Discoverable && BR/EDR Not Supported
            len(self.name) + 1, 9, self.name, # Complete local name
            3 + (len(values) * (1 + 2)), 0xFF, 0x89, 0x17, # Manufacturer data / Mf ID 0x17 0x89
                len(values)
        )

        print(values)
        for sensor_id, sensor_value in values.items():
            advertisement += struct.pack("<Be", sensor_id, sensor_value)

        print(advertisement)
        if _bleio.adapter.advertising:
            _bleio.adapter.stop_advertising()

        _bleio.adapter.start_advertising(advertisement, connectable=False, timeout=timeout)

    def broadcast_values_text(self, values, timeout = 10):
        print("Advertising through BLE")
        data_string = f"{values}"
        advertisement = struct.pack("<" "BB" "B"
                                        "BB" "6s"
                                        "BB" "BB" f"{len(data_string)}s",
            2, 1, 6, # Flags / LE General Discoverable && BR/EDR Not Supported
            len(self.name) + 1, 9, self.name, # Complete local name
            3 + len(data_string), 0xFF, 0x89, 0x17, # Manufacturer data / Mf ID 0x17 0x89
                data_string
        )

        print(values)
        if _bleio.adapter.advertising:
            _bleio.adapter.stop_advertising()

        _bleio.adapter.start_advertising(advertisement, connectable=False, timeout=timeout)

    def stop_broadcasting(self):
        _bleio.adapter.stop_advertising()

