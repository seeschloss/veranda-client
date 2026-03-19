BOARD=xiao # xiao/wroom
MODEM=quectel # quectel/simcom/wifi

ACTION=build

all: build

run: ACTION=run
run: build

build: build-board-$(BOARD)

build-board-xiao:
	IDF_TARGET=esp32s3
	cargo $(ACTION) --release --target xtensa-esp32s3-espidf --features board-xiao,modem-$(MODEM)

build-board-wroom:
	IDF_TARGET=esp32s3
	cargo $(ACTION) --release --target xtensa-esp32s3-espidf --features board-wroom,modem-$(MODEM)

