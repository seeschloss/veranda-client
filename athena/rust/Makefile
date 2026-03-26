# xiao/wroom
BOARD=xiao

# quectel/simcom/wifi
MODEM=quectel

ACTION=build

all: build

run: ACTION=run
run: build

build: build-board-$(BOARD)

build-board-xiao:
	IDF_TARGET=esp32s3
	cargo $(ACTION) --release --target xtensa-esp32s3-espidf --features board-xiao,modem-$(MODEM)
	espflash save-image --chip esp32s3 target/xtensa-esp32s3-espidf/release/athena release_firmware_xiao_$(MODEM).bin

build-board-wroom:
	IDF_TARGET=esp32s3
	cargo $(ACTION) --release --target xtensa-esp32s3-espidf --features board-wroom,modem-$(MODEM)
	espflash save-image --chip esp32s3 target/xtensa-esp32s3-espidf/release/athena release_firmware_wroom.bin

