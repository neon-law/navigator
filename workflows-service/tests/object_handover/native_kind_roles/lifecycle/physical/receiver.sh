#!/bin/sh
# Only the uniquely labelled acceptance Pod runs this supervisor. No live slot name.
set -eu
slot="$1"
case "$slot" in physical_[0-9a-f]*) ;; *) exit 2;; esac
[ "${#slot}" -eq 41 ]
mkdir -p /wal/spool
pg_receivewal --slot="$slot" --create-slot
pg_receivewal --slot="$slot" --directory=/wal/spool --synchronous > /work/receiver.log 2>&1 &
receiver=$!
printf '%s\n' "$receiver" > /work/receiver.pid
trap 'kill -TERM "$receiver" 2>/dev/null || :; wait "$receiver" 2>/dev/null || :; exit 143' TERM INT
if wait "$receiver"; then :; else
  test -f /work/receiver-stop-request
fi
test -f /work/receiver-stop-request
touch /work/receiver-drained
# The test invokes base/restore scripts through exec; this Pod has no Service.
while :; do sleep 10 & wait "$!"; done
