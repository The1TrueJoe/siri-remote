#!/bin/sh
# Publish every remote event to MQTT, one message per event.
#
#   siri-events listen DC:2B:2A:E0:18:71 | ./mqtt_glue.sh
#
# Shows that glue needs no programming language in particular: the event stream
# is line-delimited JSON, so jq and a CLI publisher are enough.
set -eu

BROKER=${BROKER:-localhost}
TOPIC=${TOPIC:-home/siri-remote}

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
command -v mosquitto_pub >/dev/null || { echo "mosquitto_pub is required" >&2; exit 1; }

while IFS= read -r line; do
  kind=$(printf '%s' "$line" | jq -r '.event // empty')
  [ -n "$kind" ] || continue
  mosquitto_pub -h "$BROKER" -t "$TOPIC/$kind" -m "$line"
done
