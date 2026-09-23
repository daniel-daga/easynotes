#!/bin/sh
# Run from DSM Task Scheduler as a user allowed to run Docker (typically root).
set -eu

backup_dir="${BACKUP_DIR:-/volume1/docker/easynotes-backups}"
mkdir -p "$backup_dir"
stamp=$(date +%Y%m%d-%H%M%S)
sql="$backup_dir/.easynotes-$stamp.sql"
archive="$backup_dir/easynotes-$stamp.sql.gz"

cleanup() {
    rm -f "$sql" "$archive.tmp"
}
trap cleanup EXIT HUP INT TERM

docker exec easynotes-db pg_dump -U easynotes easynotes > "$sql"
gzip -c "$sql" > "$archive.tmp"
mv "$archive.tmp" "$archive"
rm -f "$sql"
trap - EXIT HUP INT TERM
printf 'Wrote %s\n' "$archive"
