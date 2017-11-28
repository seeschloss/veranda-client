#!/bin/bash

CONFIG_FILE="$HOME/.verandarc"

function help() {
	return
}

function log() {
	echo "$@"
}

function http_query() {
	curl --silent -H "X-Api-Key: ${api_key}" "$@"
}

function config() {
	FILE="$1"
	if test ! -e "$FILE"; then
		echo > "$FILE"
	fi

	OLDIFS=$IFS
	IFS="="; while read KEY VALUE; do
		KEY="${KEY// }"
		KEY="${KEY/-/_}"
		VALUE="${VALUE/ }"
		if test -n "$KEY" -a -n "$VALUE"; then
			export $KEY=$VALUE
		fi
	done < "$FILE"
	IFS=$OLDIFS

	return
}

function handle_sensor() {
	NAME="$1"
	ID="$2"
	CMD="$3"

	log "Retrieving value for sensor $NAME, id #$ID..."
	VALUE=$(eval $CMD)

	http_query "https://veranda.seos.fr/data/sensor/${ID}?value=${VALUE}"
}

function handle_device() {
	NAME="$1"
	ID="$2"
	CMD_ON="$3"
	CMD_OFF="$4"

	log "Retrieving action for device $NAME, id #$ID..."

	RESULT=$(http_query "https://veranda.seos.fr/data/device/${ID}")
	echo "'$RESULT'"

	if test "$RESULT" = "on"; then
		eval $CMD_ON
		if test "$?" -eq 0; then
			http_query "https://veranda.seos.fr/data/device/${ID}?state=on"
		else
			http_query "https://veranda.seos.fr/data/device/${ID}?state=error"
		fi
	elif test "$RESULT" = "off"; then
		eval $CMD_OFF
		if test "$?" -eq 0; then
			http_query "https://veranda.seos.fr/data/device/${ID}?state=off"
		else
			http_query "https://veranda.seos.fr/data/device/${ID}?state=error"
		fi
	else
		http_query "https://veranda.seos.fr/data/device/${ID}?state=nop"
	fi
}

config "$CONFIG_FILE"

for sensor in $sensors; do
	SENSOR_ID_VAR="sensor_${sensor}_id"
	SENSOR_ID=${!SENSOR_ID_VAR}

	SENSOR_CMD_VAR="sensor_${sensor}_cmd"
	SENSOR_CMD=${!SENSOR_CMD_VAR}

	if test -n "$SENSOR_ID" -a -n "$SENSOR_CMD"; then
		handle_sensor "$sensor" "$SENSOR_ID" "$SENSOR_CMD"
	fi
done

for device in $devices; do
	DEVICE_ID_VAR="device_${device}_id"
	DEVICE_ID=${!DEVICE_ID_VAR}

	DEVICE_CMD_VAR_ON="device_${device}_cmd_on"
	DEVICE_CMD_ON=${!DEVICE_CMD_VAR_ON}

	DEVICE_CMD_VAR_OFF="device_${device}_cmd_off"
	DEVICE_CMD_OFF=${!DEVICE_CMD_VAR_OFF}

	if test -n "$DEVICE_ID" -a -n "$DEVICE_CMD_ON"; then
		handle_device "$device" "$DEVICE_ID" "$DEVICE_CMD_ON" "$DEVICE_CMD_OFF"
	fi
done
