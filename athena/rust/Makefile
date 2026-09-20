# xiao/wroom
BOARD=xiao

# quectel/simcom/wifi
MODEM=quectel

ACTION=build

all: build

run: ACTION=run
run: build

build: build-board-$(BOARD)

build-board-esp32cam:
	IDF_TARGET=esp32
	MCU=esp32 cargo $(ACTION) --release --target xtensa-esp32-espidf --features board-esp32cam,modem-$(MODEM)
	espflash save-image --chip esp32 target/xtensa-esp32-espidf/release/athena release_firmware_esp32cam_$(MODEM).bin

build-board-xiao:
	IDF_TARGET=esp32s3
	cargo $(ACTION) --release --target xtensa-esp32s3-espidf --features board-xiao,modem-$(MODEM)
	espflash save-image --chip esp32s3 target/xtensa-esp32s3-espidf/release/athena release_firmware_xiao_$(MODEM).bin

build-board-wroom:
	IDF_TARGET=esp32s3
	cargo $(ACTION) --release --target xtensa-esp32s3-espidf --features board-wroom,modem-$(MODEM)
	espflash save-image --chip esp32s3 target/xtensa-esp32s3-espidf/release/athena release_firmware_wroom_$(MODEM).bin

