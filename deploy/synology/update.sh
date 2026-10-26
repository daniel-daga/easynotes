#!/bin/sh
# Update the stack on the NAS in one go: back up, rebuild the images, recreate
# the containers, and wait until the new relay reports healthy.
#
# Run from anywhere as a user allowed to run Docker (typically root):
#
#   /volume1/docker/easynotes/deploy/synology/update.sh
#
# When a database exists the backup runs first, so a build or start that goes
# wrong leaves a dump and a media archive taken just before it. If the backup
# fails, nothing is rebuilt and nothing is restarted.
#
# The relay image is rebuilt, and so is the web app image when
# COMPOSE_PROFILES=web is set in .env. `db` is the stock Postgres image and is
# recreated only if its image was updated. On a first start, with no service
# container, this creates the stack. The backup is skipped only when neither a
# database container nor a database directory exists. An unreadable NAS_DATA_DIR
# aborts when there is no database container to dump.
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
# of the backup (it refuses to call that a backup). Compose resolves dotenv
# quoting, interpolation and an exported value; this parser must not guess, so
# anything it cannot read as a literal path leaves data_dir empty and the
# backup step below refuses to skip.
#
# An exported NAS_DATA_DIR wins over .env, as it does for Compose, including an
# exported empty value, which means the compose default rather than "read .env".
if [ -n "${NAS_DATA_DIR+x}" ]; then
    data_dir=$NAS_DATA_DIR
else
    data_dir=$(sed -n 's/^[[:space:]]*NAS_DATA_DIR[[:space:]]*=[[:space:]]*//p' .env |
        tail -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
    if [ -z "$data_dir" ]; then
        data_dir=$(sed -n 's/^[[:space:]]*export[[:space:]][[:space:]]*NAS_DATA_DIR[[:space:]]*=[[:space:]]*//p' .env |
            tail -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
    fi
fi
case "$data_dir" in
    \"*\") data_dir=${data_dir#\"}; data_dir=${data_dir%\"} ;;
    \'*\') data_dir=${data_dir#\'}; data_dir=${data_dir%\'} ;;
esac
# Empty after quoting is the compose file's default, exactly as Compose reads it.
case "$data_dir" in
    '') data_dir=/volume1/docker/easynotes-data ;;
esac
# Only a literal absolute path is safe to test: Compose expands `~`, shell
# escapes and interpolation, and this parser cannot. Anything else leaves the
# path empty so the backup step aborts rather than skipping a database it
# cannot see.
case "$data_dir" in
    /*) ;;
    *) data_dir= ;;
esac
case "$data_dir" in
    *'$'*|*'"'*|*"'"*|*'#'*|*' '*|*'~'*|*'\'*) data_dir= ;;
esac
if [ -z "${MEDIA_DIR:-}" ] && [ -n "$data_dir" ]; then
    export MEDIA_DIR="$data_dir/media"
fi

# Container Manager on DSM 7.1 and earlier ships `docker-compose`; 7.2 ships
# the `docker compose` plugin. Use whichever this NAS has.
compose_v2=0
if docker compose version >/dev/null 2>&1; then
    dc_compose() { docker compose "$@"; }
    compose_v2=1
elif command -v docker-compose >/dev/null 2>&1; then
    dc_compose() { docker-compose "$@"; }
else
    printf 'error: neither "docker compose" nor "docker-compose" is available\n' >&2
    exit 1
fi

# Container Manager prefixes a container's name with a hash of its own
# (`c86b..._easynotes-relay`), and it is the one that created the stack. Find
# its project in the labels of whichever service container still exists and
# pass it to every compose call below: a fresh project name would create a
# second stack, and the old containers already hold the loopback ports and the
# fixed names. Looking at `db` and `web` too matters when the relay is gone but
# the stack is not. Compose derives the name from the repository folder only
# when no service container exists at all. COMPOSE_PROJECT_NAME in .env is the
# fallback when a label is missing.
project=
existing=
for service in relay db web; do
    ids=$(docker ps -aq --filter "name=easynotes-$service\$") || {
        printf 'error: cannot query Docker for easynotes-%s containers\n' "$service" >&2
        exit 1
    }
    case "$(printf '%s\n' "$ids" | grep -c . || true)" in
        0) continue ;;
        1) ;;
        *) printf 'error: more than one easynotes-%s container; remove the extra one first:\n%s\n' "$service" "$ids" >&2; exit 1 ;;
    esac
    existing=1
    label=$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$ids" 2>/dev/null || true)
    case "$label" in
        '<no value>') label= ;;
    esac
    [ -n "$label" ] || continue
    if [ -z "$project" ]; then
        project=$label
    elif [ "$project" != "$label" ]; then
        printf 'error: easynotes-%s belongs to compose project %s, not %s; remove the stray container\n' \
            "$service" "$label" "$project" >&2
        exit 1
    fi
done
if [ -z "$project" ]; then
    project=$(sed -n 's/^[[:space:]]*COMPOSE_PROJECT_NAME[[:space:]]*=[[:space:]]*//p' .env | tail -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
fi
if [ -z "$project" ] && [ -n "$existing" ]; then
    printf 'error: cannot read the compose project of the existing easynotes containers; set COMPOSE_PROJECT_NAME in .env\n' >&2
    exit 1
fi
if [ -n "$project" ]; then
    compose() { dc_compose -p "$project" -f compose.synology.yaml "$@"; }
else
    compose() { dc_compose -f compose.synology.yaml "$@"; }
fi

backed_up=0
db_ids=$(docker ps -aq --filter 'name=easynotes-db$') || {
    printf 'error: cannot query Docker for easynotes-db containers\n' >&2
    exit 1
}
if [ -n "${DB_CONTAINER:-}" ] || [ -n "$db_ids" ]; then
    printf 'Backing up the database and attachments...\n'
    ./deploy/synology/backup.sh
    backed_up=1
elif [ -z "$data_dir" ]; then
    printf 'error: cannot resolve NAS_DATA_DIR; set it to a literal path or set DB_CONTAINER\n' >&2
    exit 1
elif [ -d "$data_dir/postgres" ]; then
    printf 'error: %s/postgres exists but no easynotes-db container; start the stack and rerun\n' "$data_dir" >&2
    exit 1
else
    printf 'No database container or data directory yet; skipping the pre-update backup.\n'
fi

if ! services=$(compose config --services); then
    printf 'error: cannot read the compose service list\n' >&2
    exit 1
fi
services=$(printf '%s\n' "$services" | tr -d '\r')

# `docker compose` v2 filters `config --services` by the active profile, but
# docker-compose v1 (which DSM 7.1 has) prints every service. Read
# COMPOSE_PROFILES itself on that path, so a disabled web service is not built:
# on an ARM or offline NAS that build fails and takes the relay update with it.
web=0
if [ "$compose_v2" = 1 ]; then
    case "
$services
" in
        *"
web
"*) web=1 ;;
    esac
else
    profiles=${COMPOSE_PROFILES-}
    if [ -z "${COMPOSE_PROFILES+x}" ]; then
        profiles=$(sed -n 's/^[[:space:]]*COMPOSE_PROFILES[[:space:]]*=[[:space:]]*//p' .env |
            tail -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
        if [ -z "$profiles" ]; then
            profiles=$(sed -n 's/^[[:space:]]*export[[:space:]][[:space:]]*COMPOSE_PROFILES[[:space:]]*=[[:space:]]*//p' .env |
                tail -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
        fi
        case "$profiles" in
            \"*\") profiles=${profiles#\"}; profiles=${profiles%\"} ;;
            \'*\') profiles=${profiles#\'}; profiles=${profiles%\'} ;;
        esac
    fi
    # v1 splits COMPOSE_PROFILES on commas and has no `*` wildcard. Matching
    # between commas keeps a profile named `web` from matching `webhook`.
    case ",$profiles," in
        *,web,*) web=1 ;;
    esac
fi

printf 'Building the relay image...\n'
compose build relay

if [ "$web" = 1 ]; then
    printf 'Building the web app image; the first build downloads Flutter and takes a while...\n'
    compose build web
fi

printf 'Starting the containers...\n'
compose up -d --remove-orphans

# The compose file's healthcheck is the relay's own `check` subcommand: the
# process is up and its database answers. Wait for that instead of reporting
# success while the new container is still starting, or crash-looping. `-a` and
# the name suffix find it whether Container Manager prefixed the name and even
# when it has already exited, which is when its log matters most.
printf 'Waiting for the relay to become healthy...\n'
relay_id=
state=
attempt=0
while [ "$attempt" -lt 45 ]; do
    relay_id=$(docker ps -aq --filter 'name=easynotes-relay$' | head -n 1 || true)
    if [ -n "$relay_id" ]; then
        state=$(docker inspect -f \
            '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' \
            "$relay_id" 2>/dev/null || true)
    fi
    case "$state" in
        healthy) break ;;
        unhealthy | exited | dead | removing | missing) break ;;
    esac
    attempt=$((attempt + 1))
    sleep 2
done

if [ "$state" != healthy ]; then
    printf 'error: the relay is %s after %s seconds\n' \
        "${state:-not running}" "$((attempt * 2))" >&2
    if [ -n "$relay_id" ]; then
        docker logs --tail 50 "$relay_id" >&2 2>&1 || true
    else
        docker ps -a --filter 'name=easynotes-relay$' >&2 || true
    fi
    exit 1
fi

compose ps
if [ "$backed_up" = 1 ]; then
    printf 'Update complete. Backups are in %s\n' "${BACKUP_DIR:-/volume1/docker/easynotes-backups}"
else
    printf 'Update complete. No backup was taken; there was no database to back up.\n'
fi
