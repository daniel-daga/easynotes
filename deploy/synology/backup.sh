#!/bin/sh
# Run from DSM Task Scheduler as a user allowed to run Docker (typically root).
#
# A dump is checked before it counts, and only then are old ones pruned: a
# failing pg_dump must never be the run that also deletes the last good copy.
# The exit status is the alert. DSM Task Scheduler can email on a non-zero exit
# ("Send run details by email" → "only when the script terminates abnormally").
set -eu

backup_dir="${BACKUP_DIR:-/volume1/docker/easynotes-backups}"
# How many days of dumps to keep here. The off-site copy (Hyper Backup) has its
# own retention; this only bounds what fills the NAS.
keep_days="${BACKUP_KEEP_DAYS:-30}"

mkdir -p "$backup_dir"
stamp=$(date +%Y%m%d-%H%M%S)
sql="$backup_dir/.easynotes-$stamp.sql"
archive="$backup_dir/easynotes-$stamp.sql.gz"

cleanup() {
    rm -f "$sql" "$archive.tmp"
}
trap cleanup EXIT HUP INT TERM

docker exec easynotes-db pg_dump -U easynotes easynotes > "$sql"
# pg_dump writes this as its last line only when it finished. A dump cut short
# by a full disk or a killed container is otherwise a valid-looking file.
if ! tail -n 5 "$sql" | grep -q '^-- PostgreSQL database dump complete'; then
    printf 'error: %s is incomplete; nothing was pruned\n' "$sql" >&2
    exit 1
fi
gzip -c "$sql" > "$archive.tmp"
gzip -t "$archive.tmp"
mv "$archive.tmp" "$archive"
rm -f "$sql"
trap - EXIT HUP INT TERM
printf 'Wrote %s\n' "$archive"

# Only the script's own archives, and never the one just written.
find "$backup_dir" -maxdepth 1 -type f -name 'easynotes-*.sql.gz' \
    -mtime +"$keep_days" ! -name "$(basename "$archive")" -print -exec rm -f {} +
