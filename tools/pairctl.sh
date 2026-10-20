#!/bin/bash
# start|stop|log the background pairer, without pkill self-matching the caller.
PIDF=/tmp/pairer.pid
LOG=/tmp/pair.log

stop() {
  if [ -f "$PIDF" ]; then
    kill "$(cat "$PIDF")" 2>/dev/null
    sudo kill "$(cat "$PIDF")" 2>/dev/null
    rm -f "$PIDF"
  fi
  # match only the python interpreter line, never this script's own cmdline
  for p in $(pgrep -f '^python3 -u /home/pi/pair_remote'); do sudo kill "$p" 2>/dev/null; done
}

case "$1" in
  start)
    stop; sleep 1; rm -f "$LOG"
    sudo nohup python3 -u /home/pi/pair_remote.py "$2" --timeout "${3:-300}" \
      > "$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDF"
    sleep 4
    echo "started pid=$(cat $PIDF)"
    cat "$LOG"
    ;;
  stop) stop; echo stopped ;;
  log)  cat "$LOG" 2>/dev/null || echo "(no log)" ;;
  *)    echo "usage: pairctl.sh start <ADDR> [timeout] | stop | log" ;;
esac
