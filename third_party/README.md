# Vendored crates

## `flutter_rust_bridge` 2.13.0

The published crate, byte for byte, with two changes in
`src/generalized_arc/moi_arc.rs`. [`flutter_rust_bridge.patch`](flutter_rust_bridge.patch)
is that change and nothing else. The root `Cargo.toml` puts this copy in place of the
registry's with `[patch.crates-io]`, and keeps it out of the workspace with `exclude`,
so `cargo clippy --workspace`, `cargo fmt --all` and `cargo test --workspace` leave
it alone. Cargo does not cap a path dependency's lints the way it caps a registry
one's, so a wasm build prints the crate's own `BlobPropertyBag::type_` deprecation
warning. It is upstream's, and it fails nothing.

**What it changes.** The bridge keeps every Rust object that Dart holds in a pool
behind a `std::sync::RwLock` (its "MOI" codec). In a browser that lock is also taken
on the page's main thread: Dart clones an object's handle there for every call it
makes on it, a `#[frb(sync)]` call drops its handle there, and so does Dart's
finalizer. When a Web Worker holds the lock at that moment, the main thread waits
with `Atomics.wait`, which a main thread is not allowed to do, and the synchronous
call throws `Atomics.wait cannot be called in this context`. The patch takes the
pool's lock the way [`app/rust/src/lock.rs`](../app/rust/src/lock.rs) takes the
app's own locks: on wasm, by asking until it is free. Nothing that holds the lock
waits for anything while it does, so the spin is as short as a map operation.
This lock change leaves native builds unchanged.

The second change releases a dropping `MoiArc`'s cached `Arc` before decrementing
the pool count. Previously, another thread could observe a pool count of one in
`try_unwrap` while the dropping handle still held its cached `Arc`, causing
`Arc::into_inner(...).unwrap()` to panic. This ordering fix applies to all targets.
`cargo test -p rust_lib_easynotes moi_arc_tests` exercises the vendored macro and
checks this ordering with the pool lock held, plus raw-handle ownership transfer.
It does not establish the cause of ADR 0002's missing-ID/double-release panic.

[ADR 0002](../docs/adr/0002-web-core.md) ("Not done, and honest about it") has the
stack that showed it, and why the pool was patched rather than swapped for the
`Nom` codec. That codec needs no lock, but it would turn the other open bridge
failure, a double release, from a logged panic into a use-after-free.

**On an upgrade.** Check the new release's `moi_arc.rs` first. If it no longer
takes a blocking lock on the web **and** releases cached Arcs before decrementing
the pool count, delete this directory, the `[patch.crates-io]`
entry and the `exclude`, and you are done. Otherwise:

```sh
rm -r third_party/flutter_rust_bridge
cp -r ~/.cargo/registry/src/*/flutter_rust_bridge-<version> third_party/flutter_rust_bridge
rm -f third_party/flutter_rust_bridge/.cargo-ok third_party/flutter_rust_bridge/Cargo.lock
cd third_party/flutter_rust_bridge && patch -p1 < ../flutter_rust_bridge.patch
```

`patch` telling you a hunk failed means the pool code moved. Apply the same idea
by hand, then regenerate the `.patch` file. The Dart package's version in
`app/pubspec.yaml` and `flutter_rust_bridge_codegen` must move with it, as they
always have.
