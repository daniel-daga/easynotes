# EasyNotes app

The Flutter interface for EasyNotes. The shared Rust core is connected through
the bridge in [`rust/`](rust/); generated Dart bindings live in `lib/src/rust/`.

See the [project README](../README.md#getting-started) for setup and platform
build guides, and [CONTRIBUTING.md](../CONTRIBUTING.md) for testing and bridge
generation. Run `just` recipes from the repository root.
