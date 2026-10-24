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
# Attachment bytes (ADR 0027). Archived after the dump, never before: an
# object the dump says is committed was on disk before the dump began.
media_dir="${MEDIA_DIR:-/volume1/docker/easynotes-data/media}"

mkdir -p "$backup_dir"
stamp=$(date +%Y%m%d-%H%M%S)
sql="$backup_dir/.easynotes-$stamp.sql"
archive="$backup_dir/easynotes-$stamp.sql.gz"
media="$backup_dir/easynotes-media-$stamp.tar.gz"

# Container Manager prefixes a container's name with a hash of its own
# (`c86b..._easynotes-db`), so the compose name is the end of the real name.
# DB_CONTAINER overrides this when more than one running container matches.
db=${DB_CONTAINER:-$(docker ps -q --filter 'name=easynotes-db$' || true)}
case "$(printf '%s\n' "$db" | grep -c . || true)" in
    1) ;;
    0) printf 'error: no running easynotes-db container; set DB_CONTAINER\n' >&2; exit 1 ;;
    *) printf 'error: more than one running easynotes-db container; set DB_CONTAINER:\n%s\n' "$db" >&2; exit 1 ;;
esac

cleanup() {
    rm -f "$sql" "$archive.tmp" "$media.tmp"
}
trap cleanup EXIT HUP INT TERM

docker exec "$db" pg_dump -U easynotes easynotes > "$sql"
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
if [ -d "$media_dir" ]; then
    tar -czf "$media.tmp" -C "$(dirname "$media_dir")" "$(basename "$media_dir")"
    gzip -t "$media.tmp"
    mv "$media.tmp" "$media"
    printf 'Wrote %s\n' "$media"
else
    printf 'error: no media directory at %s; set MEDIA_DIR\n' "$media_dir" >&2
    exit 1
fi
trap - EXIT HUP INT TERM
printf 'Wrote %s\n' "$archive"

# Only the script's own archives, and never the one just written.
find "$backup_dir" -maxdepth 1 -type f \
    \( -name 'easynotes-*.sql.gz' -o -name 'easynotes-media-*.tar.gz' \) \
    -mtime +"$keep_days" ! -name "$(basename "$archive")" ! -name "$(basename "$media")" \
    -print -exec rm -f {} +
