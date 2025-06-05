import time
import supervisor
import busio
import re

_TICKS_PERIOD = const(1<<29)
_TICKS_MAX = const(_TICKS_PERIOD-1)
_TICKS_HALFPERIOD = const(_TICKS_PERIOD//2)

def ticks_add(ticks, delta):
    "Add a delta to a base number of ticks, performing wraparound at 2**29ms."
    return (ticks + delta) % _TICKS_PERIOD

def ticks_diff(ticks1, ticks2):
    "Compute the signed difference between two ticks values, assuming that they are within 2**28 ticks"
    diff = (ticks1 - ticks2) & _TICKS_MAX
    diff = ((diff + _TICKS_HALFPERIOD) & _TICKS_MAX) - _TICKS_HALFPERIOD
    return diff

def ticks_less(ticks1, ticks2):
    "Return true iff ticks1 is less than ticks2, assuming that they are within 2**28 ticks"
    return ticks_diff(ticks1, ticks2) < 0


class HTTP_EC200A:
    uart = False
    additional_headers = []

    def __init__(self, uart, headers = []):
        self.uart = uart
        self.additional_headers = headers

    def debug(self, message, end="\n"):
        print(message, end=end)

    def set_uart_speed(self, speed):
        supported_rates = (4800,9600,19200,38400,57600,115200,230400,460800,921600,1000000)

        if speed in supported_rates:
            self.debug(f"Trying {speed} baud mode")
            self.send_command(f"AT+IPR={speed}")
            self.uart.baudrate = speed
            return True
        else:
            return False

    def send_command(self, command, timeout=1000, sleep=100, expect="", echo=True, ignore_URCs=True, data="", chunksize=1000000):
        # Vider le buffer avant d'envoyer une nouvelle commande
        self.uart.reset_input_buffer()

        if echo:
            self.debug(f"Sending command: {command}")
        else:
            self.debug(f"Sending command: <redacted> ({type(command)})")

        if type(command) is str:
            command += "\r"

        bytes_sent = 0
        data_size = len(command)
        start_send_time = time.monotonic()
        chunk_size = 1024
        delay_between_chunks = 0.001
        try:
            while bytes_sent < data_size:
                # Calculate chunk boundaries
                start_idx = bytes_sent
                # Make sure end_idx doesn't exceed data_size
                end_idx = min(bytes_sent + chunk_size, data_size)

                # Get the chunk slice (works for bytes and bytearray)
                chunk = command[start_idx:end_idx]

                if not chunk:
                    # This case should ideally not be reached if logic is correct
                    self.debug("Chunk calculation resulted in empty chunk before completion.")
                    break

                # Write the chunk to UART
                # uart.write() in CircuitPython is generally blocking and writes the whole buffer provided
                self.uart.write(chunk)
                bytes_sent += len(chunk) # Increment by the actual chunk length sent

                # Log progress periodically to avoid flooding logs for large files
                # e.g., Log every 20 chunks or upon completion
                log_interval = chunk_size * 20
                if bytes_sent == data_size or bytes_sent % log_interval < chunk_size:
                     self.debug(f"Sent {bytes_sent}/{data_size} bytes")

                # Apply delay if configured (important without hardware flow control)
                if delay_between_chunks and delay_between_chunks > 0:
                    time.sleep(delay_between_chunks)

            # After loop completion
            elapsed_time = time.monotonic() - start_send_time
            self.debug(f"Finished sending {bytes_sent} bytes in {elapsed_time:.2f} seconds.")

            # Final check: Did we send the expected amount?
            if bytes_sent != data_size:
                self.debug(f"Data transmission incomplete! Expected {data_size}, Sent {bytes_sent}")

        except Exception as e:
            self.debug(f"Error during data chunk sending: {e}")

        self.debug(f" command sent")

        start_time = supervisor.ticks_ms()
        buffer = b''
        response_lines = []
        URCs = ''
        while (ticks_diff(supervisor.ticks_ms(), start_time) < timeout):
            if self.uart.in_waiting:
                buffer += self.uart.read(self.uart.in_waiting)

                while b"\r\n" in buffer:
                    line, buffer = buffer.split(b"\r\n", 1)
                    try:
                        line_str = line.decode("ascii").strip()
                    except UnicodeError:
                        line_str = line.hex()

                    if not line_str:
                        continue

                    self.debug(f"<<< {line_str}")

                    response_lines.append(line_str)

                    if expect and re.match(expect, line_str):
                        return (line_str, response_lines)
                    elif line_str in ("OK", "ERROR"):
                        return (line_str, response_lines)
                    elif line_str in ("CONNECT") and data != "":
                        return self.send_command(data, timeout=timeout, echo=False)
                    elif line_str in ("CONNECT") and data == "":
                        return (line_str, response_lines)


        return ("", [])


    def send_command2(self, command, timeout=1000, sleep=100, expect="", echo=True, ignore_URCs=True, data=""):
        # Vider le buffer avant d'envoyer une nouvelle commande
        self.uart.reset_input_buffer()

        if type(command) is str:
            command += "\r"

        for chunk in range(0, len(command), 10000):
            self.uart.write(command[chunk: chunk + 10000])
            self.debug(".", end="")
            time.sleep(sleep / 1000)

        self.debug(" command sent")

        start_time = supervisor.ticks_ms()
        response = ''
        URCs = ''
        while (ticks_diff(supervisor.ticks_ms(), start_time) < timeout):
            if self.uart.in_waiting:
    #            line = self.uart.readline()
    #
    #            if (line.startswith("+") or line.startswith("SMS DONE")) and ignore_URCs:
    #                # Message non sollicité ("URC") ?
    #                URCs += line
    #            else:
    #                response += line.strip() + "\n"

                string = self.uart.read(self.uart.in_waiting)
                try:
                    # Pourquoi est-ce qu'on reçoit parfois des "\xFE", aucune idée
                    response += string.replace(b'\xfe', b'').decode('ascii')
                except:
                    response += string.hex()

                time.sleep(sleep / 1000)

            lines = response.split("\r\n")
            response = ""

            for line in lines:
                if ignore_URCs and (line.startswith("+") or line.startswith("SMS DONE")):
                    URCs += line + "\r\n"
                else:
                    response += line + "\r\n"

            if expect != "":
                r = re.compile(expect)
                if r.match(response.strip()):
                    response += self.uart.read(self.uart.in_waiting).decode('ascii')
                    break
            elif response.strip() == 'OK' or response.strip() == 'ERROR':
                response += self.uart.read(self.uart.in_waiting).decode('ascii')
                break
            elif data != "" and response.strip() == 'CONNECT':
                response += self.uart.read(self.uart.in_waiting).decode('ascii')
                break

        try:
            response = response.strip()
        except UnicodeError:
            response = response.strip()

        if not echo:
            command = "<redacted>"

        self.debug(f"UART command: {command}")
        self.debug(f"    Execution time: {ticks_diff(supervisor.ticks_ms(), start_time)} ms")
        self.debug(f"    Response: {response}")
        if len(URCs):
            URCs = URCs.strip()
            self.debug(f"    URCs: {URCs}")

        if len(data) > 0:
            return self.send_command2(data, timeout=timeout, echo=False)
        else:
            return response

    def send_file(self, url, data):
        self.send_command(f'AT+QHTTPSTOP')

        if url.startswith("https"):
            self.send_command('AT+QHTTPCFG="sslctxid",1')
            self.send_command('AT+QSSLCFG="sslversion",1,4')
            self.send_command('AT+QSSLCFG="ciphersuite",1,0xFFFF')
            self.send_command('AT+QSSLCFG="seclevel",1,0')
            self.send_command('AT+QSSLCFG="sni",1,1')

        self.send_command(f'AT+QHTTPURL={len(url)},1', data=url)

        for header in self.additional_headers:
            self.send_command(f'AT+QHTTPCFG="reqheader/add","{header[0]}","{header[1]}"')

        self.send_command(f'AT+QHTTPCFG="reqheader/add","Content-Type","image/jpeg"')

        length = len(data)
        return self.send_command(f'AT+QHTTPPOST={length},150,150', data=data, timeout=150000)

    def send_file_upl(self, url, data):
        self.send_command(f'AT+QFDEL="UFS:img.dat"')
        self.send_command(f'AT+QFUPL="UFS:img.dat",{len(data)},60,1')

        ack_char = b"A"
        # Send data in 1024-byte chunks, wait for 'A'
        chunk_size = 1024
        for i in range(0, len(data), chunk_size):
            chunk = data[i:i+chunk_size]
            self.uart.write(chunk)

            is_last_chunk = i + chunk_size >= len(data)
            if len(chunk) < chunk_size:
                break  # Done! Don't wait for ACK

            # Wait for 'A' after this chunk
            ack_start = time.monotonic()
            while True:
                if self.uart.in_waiting:
                    ack = self.uart.read(1)

                    # Accept the full 'A'
                    if ack.endswith(ack_char):
                        break
                                    # Skip over noise like \r or \n
                    elif ack[-1:] in b"\r\n":
                        continue

                                    # Unexpected character?
                    elif len(ack) > 5:
                        raise RuntimeError(f"Unexpected ACK sequence: {ack}")
                if time.monotonic() - ack_start > 10:
                    raise RuntimeError(f"Timeout waiting for ACK after chunk {i//chunk_size}")

        # Get final OK or ERROR
        final = b""
        start = time.monotonic()
        while time.monotonic() - start < 10:
            if self.uart.in_waiting:
                final += self.uart.read(self.uart.in_waiting)
                print("[FINAL] Chunk:", final.decode("ascii"))
            if b"OK" in final or b"ERROR" in final:
                print("[FINAL] ", final.decode("ascii"))
                break

        time.sleep(2)

        print(f"URL: {url}")
        self.send_command(f'AT+QHTTPURL={len(url)},1', data=url)

        for header in self.additional_headers:
            self.send_command(f'AT+QHTTPCFG="reqheader/add","{header[0]}","{header[1]}"')

        self.send_command(f'AT+QHTTPCFG="reqheader/add","Content-Type","image/jpeg"')

        result = self.send_command(f'AT+QHTTPPOSTFILE="UFS:img.dat"')
        return result


    def send_http_get(self, url, read_timeout=5):
        self.send_command(f'AT+QHTTPSTOP')

        if url.startswith("https"):
            self.send_command('AT+QHTTPCFG="sslctxid",1')
            self.send_command('AT+QSSLCFG="sslversion",1,4')
            self.send_command('AT+QSSLCFG="ciphersuite",1,0xFFFF')
            self.send_command('AT+QSSLCFG="seclevel",1,0')
            self.send_command('AT+QSSLCFG="sni",1,1')

        self.send_command(f'AT+QHTTPURL={len(url)},1', data=url)

        for header in self.additional_headers:
            self.send_command(f'AT+QHTTPCFG="reqheader/add","{header[0]}","{header[1]}"')

        self.send_command(f'AT+QHTTPGET=10')
        result, lines = self.send_command(f'AT+QHTTPREAD={read_timeout}')
        return "\r\n".join(lines)

    def send_http_post_json(self, url, data):
        self.send_command(f'AT+QHTTPSTOP')

        if url.startswith("https"):
            self.send_command('AT+QHTTPCFG="sslctxid",1')
            self.send_command('AT+QSSLCFG="sslversion",1,4')
            self.send_command('AT+QSSLCFG="ciphersuite",1,0xFFFF')
            self.send_command('AT+QSSLCFG="seclevel",1,0')
            self.send_command('AT+QSSLCFG="sni",1,1')

        self.send_command(f'AT+QHTTPURL={len(url)},1', data=url)

        for header in self.additional_headers:
            self.send_command(f'AT+QHTTPCFG="reqheader/add","{header[0]}","{header[1]}"')

        self.send_command(f'AT+QHTTPCFG="reqheader/add","Content-Type","application/json"')
        self.send_command(f'AT+QHTTPPOST={len(data)},60', data=data, timeout=5000)


    def init_modem(self, baudrate=115200, timeout=10000):
        # On attend que le modem soit prêt
        response, lines = self.send_command("ATE0")
        start_time = supervisor.ticks_ms()
        while (ticks_diff(supervisor.ticks_ms(), start_time) < timeout) and response != "OK":
            time.sleep(0.5)
            response, lines = self.send_command("ATE0")

        # Si le modem n'est pas encore prêt après le timeout, on doit essayer des trucs
        if response != "OK":
            return False

        # On ne veut pas de message non sollicités
        self.send_command("AT+CGEREP=1,0")
        self.send_command("AT+CEREG=0")
        self.send_command("AT+CGREG=0")

        if baudrate > 115200:
            # On essaie de passer en connexion un peu plus rapide...
            # je pense que pour dépasser 460800 il va falloir utiliser CTS, DTC, ce genre de trucs
            # En fait, ce n'est pas forcément nécessaire, j'ai là un module qui support 921600 sans problème
            self.set_uart_speed(baudrate)

        return self.network_registration()

    def send_sms(self, number, text):
        self.send_command(f"AT+CMGF=1")
        self.send_command(f"AT+CMGS={number}")
        self.send_command(text)
        self.send_command("\x1A")

    def network_time(self):
        response, lines = self.send_command("AT+QLTS", expect=r'.*QLTS: "([^"]*)".*', ignore_URCs=False)
        r = re.compile(r'.*QLTS: "([^"]*)".*')
        matches = r.match(response)
        if matches == None:
            return 0

        time_text = matches.groups()[0]
        r = re.compile(r'([0-9]*)/([0-9]*)/([0-9]*),([0-9]*):([0-9]*):([0-9]*)\+[0-9]*')
        matches = r.match(time_text)
        if matches == None:
            return 0

        t = matches.groups()
        time_struct = time.struct_time(
            (int(t[0]), int(t[1]), int(t[2]), int(t[3]), int(t[4]), int(t[5]), 0, -1, -1)
        )

        return time.mktime(time_struct)

    def network_registration(self, timeout=10000):
        # Activation des fonctionnalités complètes
        self.send_command("AT+CFUN=1", 9000, expect=r"OK")
        self.send_command("ATE0")

        self.debug("Checking network registration")
        response, lines = self.send_command("AT+CREG?", expect=r'.*CREG:.*$', ignore_URCs=False)
        r = re.compile(r'.*CREG: 0,[1,5].*$')
        start_time = supervisor.ticks_ms()
        while (ticks_diff(supervisor.ticks_ms(), start_time) < timeout) and not r.match(response.strip()):
            time.sleep(0.5)
            response, lines = self.send_command("AT+CREG?", expect=r'.*CREG:.*$', ignore_URCs=False)

        if not r.match(response.strip()):
            return False

        return True

    def modem_sleep(self):
        self.send_command("AT+CFUN=0", 1000)

    def modem_shutdown(self):
        self.send_command("AT+CFUN=0", 1000)
        self.send_command("AT+QCSCLK=1", 1000)

