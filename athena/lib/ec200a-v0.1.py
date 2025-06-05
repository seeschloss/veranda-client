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

    def send_command(self, command, timeout=1000, sleep=100, expect="", echo=True, ignore_URCs=True, data=""):
        # Vider le buffer avant d'envoyer une nouvelle commande
        self.uart.reset_input_buffer()

        if type(command) is str:
            command += "\r"

        for chunk in range(0, len(command), 10000):
            self.uart.write(command[chunk: chunk + 10000])
            self.debug(".", end="")
            time.sleep(sleep / 1000)

        self.debug("Command sent")

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
            return self.send_command(data, timeout=timeout, echo=False)
        else:
            return response

    def send_file(self, url, data):
        self.send_command(f'AT+QHTTPSTOP')
        self.send_command(f'AT+QHTTPURL={len(url)},1', data=url)

        for header in self.additional_headers:
            self.send_command(f'AT+QHTTPCFG="reqheader/add","{header[0]}","{header[1]}"')

        self.send_command(f'AT+QHTTPCFG="reqheader/add","Content-Type","image/jpeg"')
        self.send_command(f'AT+QHTTPPOST={len(data)},120', data=data, timeout=120000)
        self.send_command(f'AT+QHTTPCFG="reqheader/remove","Content-Type"')

    def send_http_get(self, url, read_timeout=5):
        self.send_command(f'AT+QHTTPSTOP')
        self.send_command(f'AT+QHTTPURL={len(url)},1', data=url)

        for header in self.additional_headers:
            self.send_command(f'AT+QHTTPCFG="reqheader/add","{header[0]}","{header[1]}"')

        self.send_command(f'AT+QHTTPGET=10')
        result = self.send_command(f'AT+QHTTPREAD={read_timeout}')
        return result

    def send_http_post_json(self, url, data):
        self.send_command(f'AT+QHTTPSTOP')
        self.send_command(f'AT+QHTTPURL={len(url)},1', data=url)

        for header in self.additional_headers:
            self.send_command(f'AT+QHTTPCFG="reqheader/add","{header[0]}","{header[1]}"')

        self.send_command(f'AT+QHTTPCFG="reqheader/add","Content-Type","application/json"')
        self.send_command(f'AT+QHTTPPOST={len(data)},60', data=data, timeout=5000)
        self.send_command(f'AT+QHTTPCFG="reqheader/remove","Content-Type"')


    def init_modem(self, timeout=10000):
        # On attend que le modem soit prêt
        response = self.send_command("ATE0")
        start_time = supervisor.ticks_ms()
        while (ticks_diff(supervisor.ticks_ms(), start_time) < timeout) and response != "OK":
            time.sleep(0.5)
            response = self.send_command("ATE0")

        # Si le modem n'est pas encore prêt après le timeout, on doit essayer des trucs
        if response != "OK":
            return False

        # On ne veut pas de message non sollicités
        self.send_command("AT+CGEREP=1,0")
        self.send_command("AT+CEREG=0")
        self.send_command("AT+CGREG=0")

        # On essaie de passer en connexion un peu plus rapide...
        # je pense que pour dépasser 460800 il va falloir utiliser CTS, DTC, ce genre de trucs
        #self.set_uart_speed(460800)
        self.set_uart_speed(230400)

        return self.network_registration()

    def network_registration(self, timeout=10000):
        # Activation des fonctionnalités complètes
        self.send_command("AT+CFUN=1", 9000, expect=r"OK")
        self.send_command("ATE0")

        self.debug("Checking network registration")
        response = self.send_command("AT+CREG?", expect=r'.*CREG:.*$', ignore_URCs=False)
        r = re.compile(r'.*CREG: 0,[1,5].*$')
        start_time = supervisor.ticks_ms()
        while (ticks_diff(supervisor.ticks_ms(), start_time) < timeout) and not r.match(response.strip()):
            time.sleep(0.5)
            response = self.send_command("AT+CREG?", expect=r'.*CREG:.*$', ignore_URCs=False)

        if not r.match(response.strip()):
            return False

        return True

    def modem_sleep(self):
        self.send_command("AT+CFUN=0", 1000)

    def modem_shutdown(self):
        self.send_command("AT+CFUN=0", 1000)
        self.send_command("AT+QCSCLK=1", 1000)

