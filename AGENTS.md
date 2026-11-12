# Agent instructions

EasyNotes is a public, local-first, end-to-end encrypted notes app. The product
code is the Rust core (`core/`), the relay (`relay/`), the Flutter app (`app/`)
and the bridge in `app/rust`. The security-critical core must stay testable
without Flutter or a device.

Strategy, pricing, plans and the long-form decision records live in a private
companion archive. A change that spans both repositories updates the public
code/docs here and the internal record there; internal material never moves into
this repository.

## Sources and boundaries

- Read [`CONTRIBUTING.md`](CONTRIBUTING.md), [`SECURITY.md`](SECURITY.md),
  [`docs/architecture.md`](docs/architecture.md) and
  [`docs/decisions.md`](docs/decisions.md).
- Stored formats and the wire protocol follow
  [ADR 0029](docs/decisions.md#adr-0029). A change to anything stored needs a
  migration or a fixture update; never regenerate a released fixture set.
- Keep `core/` free of Flutter and platform policy.

## Verification

- Run commands from the repository root; `just --list` lists recipes.
- Core: `just test-core`; focused test:
  `cargo test -p easynotes-core <test_filter>`.
- Relay: set `DATABASE_URL` to a Postgres 16 test database or database tests
  skip. With a shared database, use
  `cargo test -p easynotes-relay -- --test-threads=1`, as CI does: parallel media
  sweeps can delete other tests' objects. `just test-relay` does not serialize.
- App: `just test-app` builds the native bridge and relay and supplies their
  paths to Flutter. Plain `flutter test` can silently skip real-core and sync
  coverage. Without Postgres, `just test-all` runs the native suite; with
  Postgres, run `just test-core`, the serial relay command above, then
  `just test-app`. Never count skips as passes.
- `just lint` includes wasm-specific Clippy and Flutter analysis, not just host
  Rust checks. Browser-only failures can escape native tests.
- Format only changed Dart files; `just fmt` formats all of `app/lib` and
  `app/test` and can introduce unrelated SDK formatter churn.

## Bridge and browser gotchas

- After changing `app/rust/src/api`, run `just bridge`; configuration is in
  `app/flutter_rust_bridge.yaml`, generated Dart output in `app/lib/src/rust`.
- Keep codegen, the Dart bridge package and the patched vendored Rust crate in
  sync. Read `third_party/README.md` before upgrading; replacing the vendor copy
  blindly loses web locking and Arc lifetime fixes.
- Run `just setup-web` then `just build-web` before app browser recipes such as
  `just web-smoke` or `just web-converge`; these checks do not build the app.
  Set `CHROME_EXECUTABLE` when Chromium is not at the default location.
- Serve threaded WASM with `tools/web-smoke/serve.py` (COOP/COEP headers), not
  `python -m http.server`. Do not override `--wasm-pack-rustflags`: it replaces
  the bridge's threading flags; `just build-web` verifies shared memory.
- `just web-media` tests browser attachment storage/transfers without Flutter;
  it needs wasm32, `wasm-bindgen-cli` and Chromium, not the app web build.

## Publishing boundaries

- `main` is protected: push a branch and open a PR; wait for the `Rust` and
  `Flutter` checks before merging.

- Do not add internal strategy, pricing, launch copy, run logs or personal data; those
  live in the project's private engineering archive;
- Do not commit secrets, recovery phrases, keys, keystores, `.env` files or personal
  screenshots;
- Do not open a public issue for a vulnerability — use the private advisory flow in
  [`SECURITY.md`](SECURITY.md).
