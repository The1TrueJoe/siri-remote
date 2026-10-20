#!/bin/bash
# start|log wrapper for diag_write.py, with btmon capturing ATT traffic.
LOG=/tmp/diag.log
SNOOP=/tmp/diag.btsnoop
case "$1" in
  start)
    for p in $(pgrep -f '^python3 -u /home/pi/siri_remote'); do sudo kill "$p" 2>/dev/null; done
    for p in $(pgrep -f '^python3 -u /home/pi/diag_write'); do sudo kill "$p" 2>/dev/null; done
    sudo pkill -x btmon 2>/dev/null
    sleep 1; rm -f "$LOG" "$SNOOP"
    sudo nohup btmon -w "$SNOOP" >/dev/null 2>&1 </dev/null &
    sleep 1
    sudo nohup python3 -u /home/pi/diag_write.py "${2:-DC:2B:2A:D6:38:61}" \
      --connect-timeout "${3:-600}" > "$LOG" 2>&1 </dev/null &
    sleep 3; echo started; cat "$LOG"
    ;;
  log) cat "$LOG" 2>/dev/null || echo "(no log)" ;;
  att)
    sudo pkill -x btmon 2>/dev/null; sleep 1
    echo "=== ATT write/error traffic ==="
    sudo btmon -r "$SNOOP" 2>/dev/null \
      | grep -aA4 -E "ATT: Write|ATT: Error|Insufficient|Encryption Change|Security" | head -80
    ;;
  *) echo "usage: diagctl.sh start [addr] [timeout] | log | att" ;;
esac
