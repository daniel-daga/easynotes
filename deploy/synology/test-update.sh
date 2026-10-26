#!/bin/sh
# Exercises update.sh with a stub `docker` on PATH, so it builds and starts
# nothing. Each scenario copies update.sh into a throwaway tree under the
# system temp directory.
set -eu
repo=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

make_tree() {
    root=$1
    mkdir -p "$root/deploy/synology" "$root/bin"
    cp "$repo/deploy/synology/update.sh" "$root/deploy/synology/update.sh"
    chmod +x "$root/deploy/synology/update.sh"
    : > "$root/compose.synology.yaml"
    printf 'NAS_DATA_DIR=%s\n' "$root/data" > "$root/.env"
    cat > "$root/deploy/synology/backup.sh" <<'EOF'
#!/bin/sh
printf 'backup ran\n' >> "$LOG"
EOF
    chmod +x "$root/deploy/synology/backup.sh"
    cat > "$root/bin/docker" <<'EOF'
#!/bin/sh
printf 'docker %s\n' "$*" >> "$LOG"
case "$*" in
    "compose version")
        [ "${V1:-0}" = 1 ] && exit 1
        exit 0 ;;
    *"config --services"*)
        if [ "${CONFIG_FAIL:-0}" = 1 ]; then
            exit 1
        fi
        if [ "${V1:-0}" = 1 ]; then
            printf 'db relay web\n'
        else
            printf 'db\nrelay\n'
            [ "${SERVICES_WEB:-1}" = 1 ] && printf 'web\n'
        fi
        exit 0 ;;
esac
case "$*" in
    *"ps -aq --filter name=easynotes-relay"*)
        if [ "${MULTI_RELAY:-0}" = 1 ]; then
            echo fake-relay-1
            echo fake-relay-2
        elif [ -f "$RELAY_STATE" ]; then
            echo fake-relay
        fi
        exit 0 ;;
    *"ps -aq --filter name=easynotes-db"*)
        if [ "${MULTI_DB:-0}" = 1 ]; then
            echo fake-db-1
            echo fake-db-2
        elif [ "${EXISTING:-0}" = 1 ]; then
            echo fake-db
        fi
        exit 0 ;;
    *"ps -aq --filter name=easynotes-web"*)
        [ "${WEB_ONLY:-0}" = 1 ] && echo fake-web
        exit 0 ;;
    *"inspect -f"*"com.docker.compose.project"*)
        if [ "${MIXED_LABELS:-0}" = 1 ]; then
            case "$*" in
                *fake-db*) printf 'otherproject\n'; exit 0 ;;
            esac
        fi
        printf 'c86beasynotes\n'
        exit 0 ;;
    *"inspect -f"*"State.Health"*) echo healthy; exit 0 ;;
    *" up "*) : > "$RELAY_STATE"; exit 0 ;;
esac
exit 0
EOF
    chmod +x "$root/bin/docker"
    cp "$root/bin/docker" "$root/bin/docker-compose"
    chmod +x "$root/bin/docker-compose"
}

# Scenario 1: fresh install, no relay and no database.
root=$work/first
make_tree "$root"
LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
        echo "scenario 1 failed:"; cat "$root/out"; exit 1; }
grep -q 'build relay' "$root/log"
grep -q 'build web' "$root/log"
grep -q 'up -d --remove-orphans' "$root/log"
if grep -q ' -p ' "$root/log"; then
    echo "scenario 1: unexpected project flag"; exit 1
fi
if grep -q 'backup ran' "$root/log"; then
    echo "scenario 1: backup should be skipped"; exit 1
fi

# Scenario 2: an existing Container Manager project with a database.
root=$work/existing
make_tree "$root"
: > "$root/relay"
LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
        echo "scenario 2 failed:"; cat "$root/out"; exit 1; }
grep -q 'docker compose -p c86beasynotes .*build relay' "$root/log"
grep -q 'backup ran' "$root/log"

# Scenario 3: the web profile is off, so only the relay is built.
root=$work/no-web
make_tree "$root"
LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=1 SERVICES_WEB=0 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
        echo "scenario 3 failed:"; cat "$root/out"; exit 1; }
if grep -q 'build web' "$root/log"; then
    echo "scenario 3: web was built with the profile off"; exit 1
fi

# Scenario 4: only the database survives; its label still names the project.
root=$work/partial
make_tree "$root"
LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
        echo "scenario 4 failed:"; cat "$root/out"; exit 1; }
grep -q 'docker compose -p c86beasynotes .*build relay' "$root/log"
grep -q 'backup ran' "$root/log"

# Scenario 5: database data on disk but no container to dump it from.
root=$work/orphan-data
make_tree "$root"
mkdir -p "$root/data/postgres"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 5: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'exists but no easynotes-db container' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 5: built despite the missing database container"; exit 1
fi

# Scenario 6: duplicate relay containers cannot be resolved to one project.
root=$work/multi-relay
make_tree "$root"
: > "$root/relay"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 MULTI_RELAY=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 6: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'more than one easynotes-relay container' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 6: built with duplicate relays"; exit 1
fi

# Scenario 7: Compose cannot list its services.
root=$work/config-fail
make_tree "$root"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 CONFIG_FAIL=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 7: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'cannot read the compose service list' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 7: built without a service list"; exit 1
fi

# Scenario 8: a simple quoted data path is stripped, then the orphan is caught.
root=$work/quoted-data
make_tree "$root"
printf 'NAS_DATA_DIR="%s"\n' "$root/data" > "$root/.env"
mkdir -p "$root/data/postgres"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 8: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'exists but no easynotes-db container' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 8: built despite the orphaned database"; exit 1
fi

# Scenario 9: interpolation cannot be resolved, so the backup must not be skipped.
root=$work/interpolated-data
make_tree "$root"
printf 'NAS_DATA_DIR=${OTHER}/data\n' > "$root/.env"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 9: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'cannot resolve NAS_DATA_DIR' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 9: built with an unresolved data directory"; exit 1
fi

# Scenario 10: relay and db disagree about the compose project.
root=$work/mixed-labels
make_tree "$root"
: > "$root/relay"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=1 MIXED_LABELS=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 10: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'belongs to compose project otherproject' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 10: built with disagreeing project labels"; exit 1
fi

# Scenario 11: more than one database container cannot be backed up.
root=$work/multi-db
make_tree "$root"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=1 MULTI_DB=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 11: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'more than one easynotes-db container' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 11: built with duplicate databases"; exit 1
fi

# Scenario 12: only the web container survives; it still names the project.
root=$work/web-only
make_tree "$root"
LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 WEB_ONLY=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
        echo "scenario 12 failed:"; cat "$root/out"; exit 1; }
grep -q 'docker compose -p c86beasynotes .*build relay' "$root/log"
grep -q 'build web' "$root/log"
if grep -q 'backup ran' "$root/log"; then
    echo "scenario 12: backup should be skipped"; exit 1
fi

# Scenario 13: an empty quoted dotenv value means the compose default.
if [ -d /volume1/docker/easynotes-data/postgres ]; then
    echo "scenario 13 skipped: the default data directory exists on this machine"
else
    root=$work/empty-quoted
    make_tree "$root"
    printf 'NAS_DATA_DIR=""\n' > "$root/.env"
    LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
        "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
            echo "scenario 13 failed:"; cat "$root/out"; exit 1; }
    if grep -q 'cannot resolve NAS_DATA_DIR' "$root/out"; then
        echo "scenario 13: an empty quoted value should use the default"; exit 1
    fi
    if grep -q 'backup ran' "$root/log"; then
        echo "scenario 13: backup should be skipped"; exit 1
    fi
fi

# Scenario 14: an exported empty value wins over .env and means the default.
if [ -d /volume1/docker/easynotes-data/postgres ]; then
    echo "scenario 14 skipped: the default data directory exists on this machine"
else
    root=$work/empty-exported
    make_tree "$root"
    mkdir -p "$root/data/postgres"
    NAS_DATA_DIR= LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
        "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
            echo "scenario 14 failed:"; cat "$root/out"; exit 1; }
    if grep -q 'cannot resolve NAS_DATA_DIR' "$root/out"; then
        echo "scenario 14: an exported empty value should use the default"; exit 1
    fi
    if grep -q 'exists but no easynotes-db container' "$root/out"; then
        echo "scenario 14: the exported empty value should win over .env"; exit 1
    fi
    if grep -q 'backup ran' "$root/log"; then
        echo "scenario 14: backup should be skipped"; exit 1
    fi
fi

# Scenario 15: a tilde path cannot be expanded like Compose would.
root=$work/tilde-data
make_tree "$root"
printf 'NAS_DATA_DIR=~/easynotes-data\n' > "$root/.env"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 15: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'cannot resolve NAS_DATA_DIR' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 15: built with a tilde data path"; exit 1
fi

# Scenario 16: a relative path is not a literal absolute path.
root=$work/relative-data
make_tree "$root"
printf 'NAS_DATA_DIR=relative/data\n' > "$root/.env"
if LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1; then
    echo "scenario 16: expected a failure"; cat "$root/out"; exit 1
fi
grep -q 'cannot resolve NAS_DATA_DIR' "$root/out"
if grep -q 'build relay' "$root/log"; then
    echo "scenario 16: built with a relative data path"; exit 1
fi

# Scenario 17: docker-compose v1 prints all services but the profile is off.
root=$work/v1-no-profile
make_tree "$root"
LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 V1=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
        echo "scenario 17 failed:"; cat "$root/out"; exit 1; }
grep -q 'build relay' "$root/log"
if grep -q 'build web' "$root/log"; then
    echo "scenario 17: web was built with the profile off"; exit 1
fi

# Scenario 18: docker-compose v1 reads COMPOSE_PROFILES from .env.
root=$work/v1-profile
make_tree "$root"
printf 'COMPOSE_PROFILES=web\n' >> "$root/.env"
LOG=$root/log RELAY_STATE=$root/relay PATH=$root/bin:$PATH EXISTING=0 V1=1 \
    "$root/deploy/synology/update.sh" > "$root/out" 2>&1 || {
        echo "scenario 18 failed:"; cat "$root/out"; exit 1; }
grep -q 'build web' "$root/log"

echo "update.sh tests passed"
