# EasyNotes
#
# Every command here is one CI runs too, so "works on my machine" and "passes in
# CI" are the same claim. `just --list` shows them all.

# The two binaries the bridge and relay-sync tests load. Both test files *skip*
# when they are missing rather than failing, so a wrong name here reads as a
# smaller green run instead of a broken one — name them per platform.
lib_name := if os() == "windows" { "rust_lib_easynotes.dll" } else if os() == "macos" { "librust_lib_easynotes.dylib" } else { "librust_lib_easynotes.so" }
exe_suffix := if os() == "windows" { ".exe" } else { "" }

native_lib := justfile_directory() / "target/debug" / lib_name
relay_bin := justfile_directory() / "target/debug/easynotes-relay" + exe_suffix

default:
    @just --list

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

# Everything this slice produces, on every target this machine can reach.
build-all: build-core build-relay build-wasm build-linux
    @echo "built: core, relay, wasm, linux desktop"

build-core:
    cargo build --workspace

build-relay:
    cargo build -p easynotes-relay --release

# The core alone, compiled for the browser. Proves nothing native crept into it.
build-wasm:
    cargo build -p easynotes-core --target wasm32-unknown-unknown --no-default-features --features yrs-backend
    cargo build -p easynotes-core --target wasm32-unknown-unknown --no-default-features --features automerge-backend

# The Flutter Linux desktop app, over the real core.
build-linux: bridge
    cd app && flutter build linux --release

# The web app. Needs `wasm-pack` and a nightly toolchain with `rust-src`, because
# flutter_rust_bridge builds threaded WebAssembly with `-Z build-std`.
#
# No `--wasm-pack-rustflags`: the bridge's own threading flags are right, and an
# override replaces them rather than adding to them. Dropping them builds a core
# that links and then fails in the browser with "WebAssembly.Memory could not be
# cloned" (ADR 0002), so the build checks for the shared memory itself.
build-web: bridge
    cd app && flutter_rust_bridge_codegen build-web --release
    @just check-threaded-wasm
    cd app && flutter build web --release

# Did the web core come out threaded? A worker can only be handed the core's
# memory if it is shared; without it the build succeeds and the page dies.
check-threaded-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    glue='{{justfile_directory()}}/app/web/pkg/rust_lib_easynotes.js'
    if ! grep -q 'shared:true' "$glue"; then
        echo "error: $glue creates unshared memory: the threaded-WebAssembly RUSTFLAGS were lost (ADR 0002)" >&2
        exit 1
    fi

# Regenerate the Dart bindings. Run this after changing anything in app/rust/src/api.
bridge:
    cd app && flutter_rust_bridge_codegen generate

# ---------------------------------------------------------------------------
# Test
# ---------------------------------------------------------------------------

test-all: test-core test-relay test-app
    @echo "all green"

# The core and everything under it. No Flutter in the loop, by design.
test-core:
    cargo test -p easynotes-core

# The relay, including two headless cores syncing through a real socket.
# Set DATABASE_URL to include the Postgres tests; without it they skip.
test-relay:
    cargo test -p easynotes-relay

# Write this build's compatibility fixtures to compat/v<version>/ (ADR 0029),
# which every later `cargo test` reads back. Run once per release, before
# it is tagged, and commit the result; the release workflow refuses a tag
# without it. It needs pg_dump on the path and DATABASE_URL naming an *empty*
# database, since the relay's dump is part of the set; take a pg_dump of the
# same major version as CI's Postgres (16), or the dump may not restore there.
# It will not overwrite a set: COMPAT_REPLACE=1 does, and is only for a version
# not yet tagged. Without `pg_dump` 16 and an empty database, dispatch the
# Compatibility fixtures workflow instead; it writes the same directory and
# uploads it as an artifact.
compat-fixtures:
    cargo test -p easynotes-relay --test compat write_compat_fixtures -- --ignored --nocapture

# Widget tests against a fake core, tests driving the real core across the
# bridge, and the demo run from the app's own API through a real relay. The last
# two groups skip unless the binaries below are built, so they are built here.
test-app:
    cargo build -p rust_lib_easynotes
    cargo build -p easynotes-relay
    # Quoted: on Windows these are backslash paths, and an unquoted `\U` is an
    # escape to the shell — which silently yields a path with the separators
    # gone and a dlopen that can only say `error code 126`.
    cd app && EASYNOTES_NATIVE_LIB='{{native_lib}}' EASYNOTES_RELAY_BIN='{{relay_bin}}' flutter test

# Exercises the NAS update script's control flow with a stub Docker. Builds and
# starts nothing; it copies update.sh into throwaway trees under the temp dir.
test-update-script:
    sh deploy/synology/test-update.sh

# Does the web build run in a browser and keep what it is given?
#
# Builds nothing: run `just build-web` first. It needs `playwright-core`, which
# it installs beside the script, and a Chromium — set CHROME_EXECUTABLE if the
# one it should use is not the one Playwright would find.
web-smoke port="8099":
    #!/usr/bin/env bash
    set -euo pipefail
    cd '{{justfile_directory()}}/tools/web-smoke'
    [ -d node_modules ] || PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --no-fund --no-audit playwright-core
    python3 serve.py '{{justfile_directory()}}/app/build/web' {{port}} &
    server=$!
    trap 'kill $server 2>/dev/null || true' EXIT
    # The server is a socket away, not a build away.
    until curl -sfo /dev/null http://127.0.0.1:{{port}}/; do sleep 0.2; done
    node smoke.mjs http://127.0.0.1:{{port}}/

# Does a tab reach a relay, publish to it, and get acknowledged?
#
# The other half of `web-smoke`: that one asks whether a browser keeps what it
# is given, this one whether it can share it. Needs `just build-web` first, and
# starts a relay of its own; the smoke selects it in Relay settings.
web-sync-smoke port="8099":
    #!/usr/bin/env bash
    set -euo pipefail
    # The debug binary, which is the one `relay_bin` names and the one
    # `test-app` builds; `build-relay` makes a release one for a different job.
    cargo build -p easynotes-relay
    cd '{{justfile_directory()}}/tools/web-smoke'
    [ -d node_modules ] || PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --no-fund --no-audit playwright-core
    '{{relay_bin}}' &
    relay=$!
    python3 serve.py '{{justfile_directory()}}/app/build/web' {{port}} &
    server=$!
    trap 'kill $relay $server 2>/dev/null || true' EXIT
    until curl -sfo /dev/null http://127.0.0.1:8080/v1/health; do sleep 0.2; done
    until curl -sfo /dev/null http://127.0.0.1:{{port}}/; do sleep 0.2; done
    node sync.mjs http://127.0.0.1:{{port}}/

# Do two tabs, sharing one list through the real screens, converge?
#
# Two browser contexts — two devices, two accounts — share a list by invite
# link, edit it online, then both offline, and must end up with the same list.
# Needs `just build-web` first, and starts a relay like `web-sync-smoke`; each browser profile selects it in Relay settings.
web-converge port="8099":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p easynotes-relay
    cd '{{justfile_directory()}}/tools/web-smoke'
    [ -d node_modules ] || PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --no-fund --no-audit playwright-core
    '{{relay_bin}}' &
    relay=$!
    python3 serve.py '{{justfile_directory()}}/app/build/web' {{port}} &
    server=$!
    trap 'kill $relay $server 2>/dev/null || true' EXIT
    until curl -sfo /dev/null http://127.0.0.1:8080/v1/health; do sleep 0.2; done
    until curl -sfo /dev/null http://127.0.0.1:{{port}}/; do sleep 0.2; done
    node converge.mjs http://127.0.0.1:{{port}}/

# Does a deletion survive IndexedDB refusing a write partway through it?
#
# The trash is emptied with the browser's own database made to refuse the
# first write of it, then the second, and so on, and a new tab must hold
# each note whole or not at all. Needs `just build-web` first; no relay.
web-storage-failure port="8099":
    #!/usr/bin/env bash
    set -euo pipefail
    cd '{{justfile_directory()}}/tools/web-smoke'
    [ -d node_modules ] || PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --no-fund --no-audit playwright-core
    python3 serve.py '{{justfile_directory()}}/app/build/web' {{port}} &
    server=$!
    trap 'kill $server 2>/dev/null || true' EXIT
    until curl -sfo /dev/null http://127.0.0.1:{{port}}/; do sleep 0.2; done
    node storage-failure.mjs http://127.0.0.1:{{port}}/

# Receive text through the manifest's share target, then reload without
# importing it twice. Needs `just build-web` first; no relay.
web-shared-text port="8099":
    #!/usr/bin/env bash
    set -euo pipefail
    cd '{{justfile_directory()}}/tools/web-smoke'
    [ -d node_modules ] || PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --no-fund --no-audit playwright-core
    python3 serve.py '{{justfile_directory()}}/app/build/web' {{port}} &
    server=$!
    trap 'kill $server 2>/dev/null || true' EXIT
    until curl -sfo /dev/null http://127.0.0.1:{{port}}/; do sleep 0.2; done
    node shared-text.mjs http://127.0.0.1:{{port}}/

# Do a tab's attachment store and transfer queue work in a browser?
#
# IndexedDB, `fetch` with a bearer token and a `Range` header across origins,
# and the queue on the page's event loop: what `cargo test` cannot reach. No
# Flutter in it, like `bench-web`, so it needs only the `wasm32-unknown-unknown`
# target, `wasm-bindgen-cli` and a Chromium (`CHROME_EXECUTABLE`). Starts a
# relay of its own on the address the app defaults to.
web-media port="8101":
    #!/usr/bin/env bash
    set -euo pipefail
    out='{{justfile_directory()}}/target/web-media'
    cargo build -p easynotes-relay
    cargo build --release -p easynotes-web-media --target wasm32-unknown-unknown
    wasm-bindgen --target web --no-typescript --out-dir "$out" \
        '{{justfile_directory()}}/target/wasm32-unknown-unknown/release/easynotes_web_media.wasm'
    cp '{{justfile_directory()}}/tools/web-media/index.html' "$out/"
    cd '{{justfile_directory()}}/tools/web-media'
    [ -d node_modules ] || PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --no-fund --no-audit playwright-core
    '{{relay_bin}}' &
    relay=$!
    python3 '{{justfile_directory()}}/tools/web-smoke/serve.py' "$out" {{port}} &
    server=$!
    trap 'kill $relay $server 2>/dev/null || true' EXIT
    until curl -sfo /dev/null http://127.0.0.1:8080/v1/health; do sleep 0.2; done
    until curl -sfo /dev/null http://127.0.0.1:{{port}}/; do sleep 0.2; done
    node media.mjs http://127.0.0.1:{{port}}/ http://127.0.0.1:8080

lint: lint-wasm
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd app && flutter analyze

# The code a browser runs, linted as a browser gets it: for wasm32, and with
# `tools/wasm-lint/clippy.toml` forbidding the std calls that compile there and
# then panic — `thread::spawn`, `SystemTime::now` and the like. A native `#[cfg]`
# branch is not compiled for this target, so it may still use them. Adding the
# target is a no-op once it is there, and a fresh container does not have it.
lint-wasm:
    rustup target add wasm32-unknown-unknown
    CLIPPY_CONF_DIR='{{justfile_directory()}}/tools/wasm-lint' cargo clippy --target wasm32-unknown-unknown -p easynotes-core --lib --no-default-features --features automerge-backend,yrs-backend,web-store,sync-web -- -D warnings
    CLIPPY_CONF_DIR='{{justfile_directory()}}/tools/wasm-lint' cargo clippy --target wasm32-unknown-unknown -p rust_lib_easynotes --lib -- -D warnings
    CLIPPY_CONF_DIR='{{justfile_directory()}}/tools/wasm-lint' cargo clippy --target wasm32-unknown-unknown -p easynotes-web-media -- -D warnings

fmt:
    cargo fmt --all
    cd app && dart format lib test

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

# A relay on 127.0.0.1:8080, storing in memory. Set DATABASE_URL for Postgres.
run-relay:
    RUST_LOG=easynotes_relay=debug cargo run -p easynotes-relay

# Windows desktop, using the installed Visual Studio C++ toolchain.
run-windows:
    cd app && flutter run -d windows

build-windows:
    cd app && flutter build windows --release

# Uses a temporary Windows secure-storage key and SQLite directory, then puts
# the normal app back in the release output folder.
windows-storage-smoke:
    cd app && flutter build windows --release --target tool/windows_storage_smoke.dart && ../tools/windows-storage-smoke.ps1 && flutter build windows --release

# The app on Linux desktop. Choose a relay in Settings after it starts.
run-linux: bridge
    cd app && flutter run -d linux

# ---------------------------------------------------------------------------
# Measure
# ---------------------------------------------------------------------------

# Automerge against Yrs on the same workload. Prints the table in ADR 0001.
bench-crdt:
    cargo run --release -p easynotes-core --example bench_crdt

# What compaction saves, in bytes on the disk and milliseconds on an open.
# Prints the tables in ADR 0006.
bench-compaction:
    cargo run --release -p easynotes-core --example bench_compaction

# What opening a device's whole note list costs, at three sizes, and what the
# account document adds before the first note is read — with its log intact and
# once compacted. Prints the tables in ADR 0007, ADR 0008 and ADR 0012.
bench-library:
    cargo run --release -p easynotes-core --example bench_library

# The same question `bench-library` asks, asked of a browser: what reading the
# whole database out of IndexedDB and unsealing it costs a tab at three sizes.
# Prints the table in ADR 0011.
#
# Needs `wasm-bindgen-cli` and the `wasm32-unknown-unknown` target — `just
# setup-web` installs both — and a Chromium, which `CHROME_EXECUTABLE` names.
# It does not need the Flutter web build: no widget is drawn, because the
# question is what the store and the crypto cost rather than what a frame does.
bench-web port="8100":
    #!/usr/bin/env bash
    set -euo pipefail
    out='{{justfile_directory()}}/target/web-bench'
    cargo build --release -p easynotes-web-bench --target wasm32-unknown-unknown
    # `--target web`, so the page loads it as a module with no bundler in the
    # way. No `wasm-opt`: it is a size pass, the build is already `--release`,
    # and reaching for it would drag in the binaryen download that CONTRIBUTING.md
    # has to work around.
    wasm-bindgen --target web --no-typescript --out-dir "$out" \
        '{{justfile_directory()}}/target/wasm32-unknown-unknown/release/easynotes_web_bench.wasm'
    cp '{{justfile_directory()}}/tools/web-bench/index.html' "$out/"
    cd '{{justfile_directory()}}/tools/web-bench'
    [ -d node_modules ] || PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --no-fund --no-audit playwright-core
    python3 '{{justfile_directory()}}/tools/web-smoke/serve.py' "$out" {{port}} &
    server=$!
    trap 'kill $server 2>/dev/null || true' EXIT
    until curl -sfo /dev/null http://127.0.0.1:{{port}}/; do sleep 0.2; done
    node bench.mjs http://127.0.0.1:{{port}}/

# What each CRDT backend adds to a WebAssembly bundle.
bench-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    wasm=target/wasm32-unknown-unknown/release/easynotes_wasm_size.wasm
    printf "%-26s %10s %10s\n" build raw gzipped
    for features in "" "automerge-backend" "yrs-backend" "automerge-backend,yrs-backend"; do
        args=(--no-default-features)
        [ -n "$features" ] && args+=(--features "$features")
        cargo build --release -p easynotes-wasm-size --target wasm32-unknown-unknown "${args[@]}" >/dev/null 2>&1
        printf "%-26s %10s %10s\n" "${features:-crypto only}" \
            "$(numfmt --to=iec < <(stat -c%s $wasm))" \
            "$(gzip -9 -c $wasm | wc -c | numfmt --to=iec)"
    done

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

# Toolchains beyond a stable Rust and Flutter. Only needed for the web build.
setup-web:
    rustup target add wasm32-unknown-unknown
    rustup toolchain install nightly
    rustup component add rust-src --toolchain nightly
    cargo install wasm-pack --locked
    # Pre-install so wasm-pack does not try to build it with the WebAssembly
    # RUSTFLAGS still set, which fails to link for the host.
    cargo install wasm-bindgen-cli --version 0.2.128 --locked
