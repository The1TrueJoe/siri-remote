#!/bin/bash
# start|stop|log wrapper for siri_att.py (pattern lives here, not in the caller)
PIDF=/tmp/att.pid; LOG=/tmp/att.log
stop() {
  [ -f "$PIDF" ] && { sudo kill "$(cat $PIDF)" 2>/dev/null; rm -f "$PIDF"; }
  for p in $(pgrep -f '^python3 -u /home/pi/siri_att'); do sudo kill "$p" 2>/dev/null; done
  for p in $(pgrep -f '^python3 -u /home/pi/siri_remote'); do sudo kill "$p" 2>/dev/null; done
  for p in $(pgrep -f '^python3 -u /home/pi/diag_write'); do sudo kill "$p" 2>/dev/null; done
}
case "$1" in
  start)
    stop; sleep 1; rm -f "$LOG"
    # EXTRA lets callers add flags, e.g. EXTRA="--no-encrypt"
    sudo nohup python3 -u /home/pi/siri_att.py "${2:-DC:2B:2A:D6:38:61}" --raw \
      --connect-timeout "${3:-600}" $EXTRA > "$LOG" 2>&1 </dev/null &
    echo $! > "$PIDF"; sleep 4; echo "started pid=$(cat $PIDF)"; cat "$LOG" ;;
  stop) stop; echo stopped ;;
  log)  cat "$LOG" 2>/dev/null || echo "(no log)" ;;
  *) echo "usage: attctl.sh start [addr] [timeout] | stop | log" ;;
esac
