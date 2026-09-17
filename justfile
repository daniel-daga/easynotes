# EasyNotes — Slice 1
#
# Every command here is one CI runs too, so "works on my machine" and "passes in
# CI" are the same claim. `just --list` shows them all.

# The threading flags WebAssembly needs, plus the one telling `getrandom` where
# browser randomness comes from. Keep them together: dropping the threading half
# produces a build that links and then fails at runtime with
# "WebAssembly.Memory could not be cloned".
wasm_rustflags := "-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-args=--shared-memory -C link-args=--max-memory=1073741824 -C link-args=--import-memory -C link-args=--export=__heap_base -C link-args=--export=__wasm_init_tls -C link-args=--export=__tls_size -C link-args=--export=__tls_align -C link-args=--export=__tls_base --cfg getrandom_backend=\"wasm_js\""

native_lib := justfile_directory() / "target/debug/librust_lib_easynotes.so"

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
build-web: bridge
    cd app && flutter_rust_bridge_codegen build-web --release --wasm-pack-rustflags '{{wasm_rustflags}}'
    cd app && flutter build web --release

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

# Widget tests against a fake core, plus tests driving the real core across the
# bridge. The second group skips unless the native library is built.
test-app:
    cargo build -p rust_lib_easynotes
    cd app && EASYNOTES_NATIVE_LIB={{native_lib}} flutter test

lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd app && flutter analyze

fmt:
    cargo fmt --all
    cd app && dart format lib test

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

# A relay on 127.0.0.1:8080, storing in memory. Set DATABASE_URL for Postgres.
run-relay:
    RUST_LOG=easynotes_relay=debug cargo run -p easynotes-relay

run-linux: bridge
    cd app && flutter run -d linux

# ---------------------------------------------------------------------------
# Measure — decision D1
# ---------------------------------------------------------------------------

# Automerge against Yrs on the same workload. Prints the table in ADR 0001.
bench-crdt:
    cargo run --release -p easynotes-core --example bench_crdt

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
