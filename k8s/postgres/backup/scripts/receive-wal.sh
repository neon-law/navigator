#!/bin/sh
# Stream WAL from navigator-postgres into the local spool through a physical
# replication slot. The slot makes the server keep each segment until it has
# been spooled here; the server's max_slot_wal_keep_size bounds that hold.
# Connection settings (verify-full TLS, the navigator_backup login) come from
# the libpq PG* environment.
set -eu
mkdir -p /wal/spool
pg_receivewal --slot=navigator_wal_archive --create-slot --if-not-exists
exec pg_receivewal --slot=navigator_wal_archive --directory=/wal/spool --synchronous --verbose
