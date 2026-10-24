#!/bin/sh
# Restore a dump into a throwaway database, check it holds a relay's tables, and
# check that the media archive taken with it holds every attachment it records.
#
# A backup that has never been restored is a hope, not a backup. Run this after
# changing anything about backups, and on a schedule (monthly is a reasonable
# start) so a dump that stopped being restorable is found before it is needed.
# It never touches the live `easynotes` database.
#
#   restore-check.sh                  # the newest dump in BACKUP_DIR
#   restore-check.sh path/to/dump.sql.gz
#
# The media archive is the one `backup.sh` wrote with the same timestamp. Every
# object the dump records as committed must be in it at the recorded size. With
# VERIFY_DIGESTS=1 the archive is also unpacked into a scratch directory
# (TMPDIR, which then needs room for all of it) and each object's SHA-256 is
# checked against the digest it was committed under: slower, and the only check
# that proves the bytes rather than their length.
set -eu

backup_dir="${BACKUP_DIR:-/volume1/docker/easynotes-backups}"
dump="${1:-$(ls -1t "$backup_dir"/easynotes-*.sql.gz 2>/dev/null | head -n 1)}"
if [ -z "$dump" ] || [ ! -f "$dump" ]; then
    printf 'error: no dump found in %s\n' "$backup_dir" >&2
    exit 1
fi

case "$dump" in
    *easynotes-*.sql.gz) ;;
    *) printf 'error: %s is not named like a backup.sh dump\n' "$dump" >&2; exit 1 ;;
esac
stamp=$(basename "$dump" .sql.gz)
stamp=${stamp#easynotes-}
media="$(dirname "$dump")/easynotes-media-$stamp.tar.gz"
if [ ! -f "$media" ]; then
    printf 'error: no media archive %s beside %s\n' "$media" "$dump" >&2
    exit 1
fi

# Container Manager prefixes a container's name with a hash of its own
# (`c86b..._easynotes-db`), so the compose name is the end of the real name.
# DB_CONTAINER overrides this when more than one running container matches.
db=${DB_CONTAINER:-$(docker ps -q --filter 'name=easynotes-db$' || true)}
case "$(printf '%s\n' "$db" | grep -c . || true)" in
    1) ;;
    0) printf 'error: no running easynotes-db container; set DB_CONTAINER\n' >&2; exit 1 ;;
    *) printf 'error: more than one running easynotes-db container; set DB_CONTAINER:\n%s\n' "$db" >&2; exit 1 ;;
esac

scratch="easynotes_restore_check_$$"
work=$(mktemp -d)

psql() {
    docker exec -i "$db" psql -v ON_ERROR_STOP=1 -q -U easynotes "$@"
}
drop() {
    psql -d postgres -c "DROP DATABASE IF EXISTS $scratch" >/dev/null
    rm -rf "$work"
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

# What the dump says is committed: "<note>/<object> <sealed bytes> <sha256>".
# A reservation still uploading (no digest) has nothing to find yet. Nor has a
# doomed object (ADR 0027 §5): nothing lists it any more, and the relay may
# have deleted its bytes between the dump and the archive. A dump from before
# the relay could doom anything has no `doomed_at` to ask about.
doomed=
if [ "$(psql -d "$scratch" -At -c "
    SELECT count(*) FROM information_schema.columns
    WHERE table_name = 'media_objects' AND column_name = 'doomed_at'")" = 1 ]; then
    doomed='AND doomed_at IS NULL'
fi
psql -d "$scratch" -At -F ' ' -c "
    SELECT note_id || '/' || object_id, sealed_len, encode(digest, 'hex')
    FROM media_objects WHERE digest IS NOT NULL $doomed ORDER BY 1" > "$work/expected"

# What the archive holds, from its listing: "<note>/<object> <bytes>". The
# archive's paths are media/objects/<note>/<object>; parts and tmp are not
# objects. The size is the listing's third column in GNU and BusyBox tar alike.
tar -tvzf "$media" | awk '
    { path = $NF; sub(/^\.\//, "", path) }
    path ~ /^[^\/]+\/objects\/[^\/]+\/[^\/]+$/ {
        sub(/^[^\/]+\/objects\//, "", path); print path, $3
    }' | sort > "$work/archived"

# Missing, or there at another length. Objects in the archive the dump does
# not name are fine: an upload committed after the dump began is one.
awk 'NR == FNR { size[$1] = $2; next }
     !($1 in size)    { print $1 ": missing"; next }
     size[$1] != $2   { print $1 ": " size[$1] " bytes, expected " $2 }' \
    "$work/archived" "$work/expected" > "$work/wrong"

if [ "${VERIFY_DIGESTS:-0}" = 1 ] && [ ! -s "$work/wrong" ]; then
    mkdir "$work/media"
    tar -xzf "$media" -C "$work/media"
    root=$(find "$work/media" -mindepth 2 -maxdepth 2 -type d -name objects | head -n 1)
    while read -r path _ digest; do
        actual=$(sha256sum "$root/$path" | cut -d ' ' -f 1)
        if [ "$actual" != "$digest" ]; then
            printf '%s: bytes do not match the committed digest\n' "$path"
        fi
    done < "$work/expected" > "$work/wrong"
fi

if [ -s "$work/wrong" ]; then
    printf 'error: %s does not hold what %s records:\n' "$media" "$dump" >&2
    head -n 20 "$work/wrong" >&2
    printf '(%s object(s) in all)\n' "$(wc -l < "$work/wrong" | tr -d ' ')" >&2
    exit 1
fi
objects=$(wc -l < "$work/expected" | tr -d ' ')
printf 'Restored %s: %s logged envelopes\n' "$dump" "$envelopes"
if [ "${VERIFY_DIGESTS:-0}" = 1 ]; then
    printf 'Checked %s: all %s committed attachments present, digests match\n' "$media" "$objects"
else
    printf 'Checked %s: all %s committed attachments present at their size\n' "$media" "$objects"
fi
