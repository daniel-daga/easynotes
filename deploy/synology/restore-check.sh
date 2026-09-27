#!/bin/sh
# Restore a dump into a throwaway database and check it holds a relay's tables.
#
# A backup that has never been restored is a hope, not a backup. Run this after
# changing anything about backups, and on a schedule (monthly is a reasonable
# start) so a dump that stopped being restorable is found before it is needed.
# It never touches the live `easynotes` database.
#
#   restore-check.sh                  # the newest dump in BACKUP_DIR
#   restore-check.sh path/to/dump.sql.gz
set -eu

backup_dir="${BACKUP_DIR:-/volume1/docker/easynotes-backups}"
dump="${1:-$(ls -1t "$backup_dir"/easynotes-*.sql.gz 2>/dev/null | head -n 1)}"
if [ -z "$dump" ] || [ ! -f "$dump" ]; then
    printf 'error: no dump found in %s\n' "$backup_dir" >&2
    exit 1
fi

scratch="easynotes_restore_check_$$"
psql() {
    docker exec -i easynotes-db psql -v ON_ERROR_STOP=1 -q -U easynotes "$@"
}
drop() {
    psql -d postgres -c "DROP DATABASE IF EXISTS $scratch" >/dev/null
}
trap drop EXIT HUP INT TERM

psql -d postgres -c "CREATE DATABASE $scratch" >/dev/null
gunzip -c "$dump" | psql -d "$scratch" >/dev/null

# The tables every relay has. A dump of the wrong database, or of an empty one
# that was never migrated, restores cleanly and fails here.
missing=$(psql -d "$scratch" -At -c "
    SELECT string_agg(t, ', ') FROM unnest(ARRAY['note_log', 'note_head', 'invites', 'invite_requests', 'media_objects']) AS t
    WHERE to_regclass('public.' || t) IS NULL")
if [ -n "$missing" ]; then
    printf 'error: %s restored without table(s): %s\n' "$dump" "$missing" >&2
    exit 1
fi
envelopes=$(psql -d "$scratch" -At -c 'SELECT count(*) FROM note_log')
printf 'Restored %s: %s logged envelopes\n' "$dump" "$envelopes"
