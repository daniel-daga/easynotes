# Build from the repository root so Cargo can resolve the workspace and the
# relay's path dependency on easynotes-core.
FROM rust:1-bookworm AS build
WORKDIR /src

# Manifests first, so that editing a source file does not invalidate the layer
# that compiles the dependency tree. Cargo has to load every workspace member
# before it builds anything, so these paths track the `members` list in
# Cargo.toml: a new member belongs here too.
COPY Cargo.toml Cargo.lock ./
COPY core/Cargo.toml core/
COPY relay/Cargo.toml relay/
COPY tools/wasm-size/Cargo.toml tools/wasm-size/
COPY tools/web-bench/Cargo.toml tools/web-bench/
COPY tools/web-media/Cargo.toml tools/web-media/
COPY app/rust/Cargo.toml app/rust/
# A [patch.crates-io] is resolved with the workspace even when the relay never
# links the crate it replaces, so the vendored patch has to be here as well.
COPY third_party/flutter_rust_bridge third_party/flutter_rust_bridge

# Compile the dependency tree against empty crates; the real sources arrive
# below. This is the expensive layer, and it now changes only with Cargo.lock
# or a manifest. Every stub is removed in the same layer, so none can survive
# into the build below and stand in for a crate that was never compiled.
RUN set -eux; \
    crates='core relay tools/wasm-size tools/web-bench tools/web-media app/rust'; \
    for crate in $crates; do \
        mkdir -p "$crate/src"; \
        : > "$crate/src/lib.rs"; \
    done; \
    cargo build --locked --release -p easynotes-relay; \
    for crate in $crates; do rm -rf "$crate/src"; done

# The real sources. Editing any of them recompiles core and the relay and
# relinks the binary — seconds, because the dependency tree above is already
# built. `touch` because Cargo decides freshness by mtime, and a copied tree
# can carry mtimes older than the artifacts the stub build just made.
COPY core core
COPY relay relay
COPY tools/wasm-size tools/wasm-size
COPY tools/web-bench tools/web-bench
COPY tools/web-media tools/web-media
COPY app/rust app/rust
RUN set -eux; \
    find core relay tools app/rust -type f -exec touch {} +; \
    cargo build --locked --release -p easynotes-relay

FROM debian:bookworm-slim AS runtime
RUN useradd --system --uid 10001 --no-create-home relay
COPY --from=build /src/target/release/easynotes-relay /usr/local/bin/easynotes-relay
USER 10001:10001
EXPOSE 8080
ENV RELAY_ADDR=0.0.0.0:8080
ENTRYPOINT ["/usr/local/bin/easynotes-relay"]
