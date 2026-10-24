#!/bin/sh
# Update the relay on the NAS in one go: back up, rebuild the relay image,
# recreate the containers, and wait until the new relay reports healthy.
#
# Run from anywhere as a user allowed to run Docker (typically root):
#
#   /volume1/docker/easynotes/deploy/synology/update.sh
#
# The backup runs first, so a build or start that goes wrong leaves a dump and
# a media archive taken just before it. If the backup fails, nothing is rebuilt
# and nothing is restarted.
#
# Only `relay` is rebuilt: `db` is the stock Postgres image and `web` serves
# static files mounted read-only. `web` is recreated too when
# COMPOSE_PROFILES=web is set in .env, and so is `db` if its image was updated.
#
# On start the relay applies any pending numbered migration itself and logs
# "applied a schema migration" for each. There is no migration step here.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

for file in compose.synology.yaml .env; do
    if [ ! -f "$file" ]; then
        printf 'error: %s not found in %s\n' "$file" "$root" >&2
        exit 1
    fi
done

# backup.sh archives $NAS_DATA_DIR/media by default but does not read .env, so
# a project that keeps its data elsewhere would fail its media check at the end
# of the backup (it refuses to call that a backup). Derive the path from .env
# unless MEDIA_DIR is already set. Values like ${OTHER_VAR} are compose
# interpolation and are left to backup.sh's own default.
if [ -z "${MEDIA_DIR:-}" ]; then
    data_dir=$(sed -n 's/^[[:space:]]*NAS_DATA_DIR[[:space:]]*=[[:space:]]*//p' .env |
        tail -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
    case "$data_dir" in
        '' | \$*) ;; # unset, or not a literal path
        *) export MEDIA_DIR="$data_dir/media" ;;
    esac
fi

# Container Manager on DSM 7.1 and earlier ships `docker-compose`; 7.2 ships
# the `docker compose` plugin. Use whichever this NAS has.
if docker compose version >/dev/null 2>&1; then
    compose() { docker compose "$@"; }
elif command -v docker-compose >/dev/null 2>&1; then
    compose() { docker-compose "$@"; }
else
    printf 'error: neither "docker compose" nor "docker-compose" is available\n' >&2
    exit 1
fi

printf 'Backing up the database and attachments...\n'
./deploy/synology/backup.sh

printf 'Building the relay image...\n'
compose -f compose.synology.yaml build relay

printf 'Starting the new relay...\n'
compose -f compose.synology.yaml up -d --remove-orphans

# The compose file's healthcheck is the relay's own `check` subcommand: the
# process is up and its database answers. Wait for that instead of reporting
# success while the new container is still starting, or crash-looping.
printf 'Waiting for easynotes-relay to become healthy...\n'
state=
attempt=0
while [ "$attempt" -lt 45 ]; do
    state=$(docker inspect -f \
        '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' \
        easynotes-relay 2>/dev/null || true)
    case "$state" in
        healthy) break ;;
        unhealthy | exited | dead | removing | missing | '') break ;;
    esac
    attempt=$((attempt + 1))
    sleep 2
done

if [ "$state" != healthy ]; then
    printf 'error: easynotes-relay is %s after %s seconds\n' \
        "${state:-unreadable}" "$((attempt * 2))" >&2
    docker logs --tail 50 easynotes-relay >&2 2>&1 || true
    exit 1
fi

compose -f compose.synology.yaml ps
printf 'Update complete. Backups are in %s\n' "${BACKUP_DIR:-/volume1/docker/easynotes-backups}"
