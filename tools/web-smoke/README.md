# Bridge lifetime diagnostics

`bridge-lifetime.mjs` drives the generated Dart bindings against the real WASM
core without Flutter's UI. Each iteration opens a `Note`, starts both watchers,
edits while reading synchronous getters, stops the watchers, and leaves the
handle to Dart's finalizer, just as the app does. It uses an ephemeral library.

Build the web app normally first. For per-ID Rust histories, build the Rust half
in dev mode with the **compile-time** environment variable `EASYNOTES_TRACE_MOI=1`:

```powershell
cd app
$env:EASYNOTES_TRACE_MOI = '1'
flutter_rust_bridge_codegen build-web
Remove-Item Env:EASYNOTES_TRACE_MOI
Copy-Item web/pkg/rust_lib_easynotes.js build/web/pkg/
Copy-Item web/pkg/rust_lib_easynotes_bg.wasm build/web/pkg/
dart compile js -O2 tool/bridge_lifetime.dart -o build/web/bridge_lifetime.js
cd ..
python tools/web-smoke/serve.py app/build/web 8099
```

In another terminal, from the repository root:

```powershell
node tools/web-smoke/bridge-lifetime.mjs http://127.0.0.1:8099/
$env:LIFETIME_MODE = 'reopen'
node tools/web-smoke/bridge-lifetime.mjs http://127.0.0.1:8099/
$env:LIFETIME_MODE = 'pending'
node tools/web-smoke/bridge-lifetime.mjs http://127.0.0.1:8099/
```

The default `gc` mode forces GC between batches in one tab. `reopen` also replaces
the tab between batches. `pending` replaces tabs without forcing GC first, leaving
finalizers pending at teardown. `LIFETIME_ROUNDS` and `LIFETIME_BATCH` default to
30 and 100. `CHROME_EXECUTABLE` optionally selects Chromium, as in the other smoke
tests. `LIFETIME_WORKERS` overrides the page's reported hardware concurrency to
vary the bridge worker count. `LIFETIME_JS_STACKS=1` records a registration stack
for each finalizer in the capture. The runner supplies its own HTML fixture; the
compiled JS and WASM must already be served. It fails on every console error or
page exception, and checks
that finalizers do not run after detachment or run twice for one registration.
GC modes also require observing finalizer execution, so a run with no callbacks
cannot pass silently.

Compressed JSONL histories are saved under `target/bridge-lifetime/`. Each entry
identifies its page, so a late message from a closing tab cannot be mistaken for
one from the replacement. Rust `FRB_MOI` records contain a sequence number, type,
ID, operation, pool count and stack. Counts are **before** increment/decrement,
after insertion for `new`, and unavailable (`-1`) for `into_raw`. Sort by sequence
within a page; worker console events can arrive out of order. Keep `Library` and
`Note` IDs separate. Logging adds overhead and can change race timing; a clean run
does not prove the missing-ID panic fixed.

To capture the original UI/IndexedDB reproduction instead:

```powershell
$env:BRIDGE_TRACE_DIR = 'target/bridge-lifetime/storage'
node tools/web-smoke/storage-failure.mjs http://127.0.0.1:8099/
```

This preserves full console/page-error stacks and makes the previously tolerated
missing-ID panic fail the test. Without `BRIDGE_TRACE_DIR`, the existing smoke
test policy is unchanged. Rebuild the Rust half **without** `EASYNOTES_TRACE_MOI`
and rebuild Flutter before using the web build normally; unsetting the variable
does not remove diagnostics from an already compiled WASM file.
