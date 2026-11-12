# Agent instructions

EasyNotes is a public, local-first, end-to-end encrypted notes app. The product
code is the Rust core (`core/`), the relay (`relay/`), the Flutter app (`app/`)
and the bridge in `app/rust`. The security-critical core must stay testable
without Flutter or a device.

Strategy, pricing, plans and the long-form decision records live in a private
companion archive. A change that spans both repositories updates the public
code/docs here and the internal record there; internal material never moves into
this repository.

Before changing anything:

- Read [`CONTRIBUTING.md`](CONTRIBUTING.md), [`SECURITY.md`](SECURITY.md),
  [`docs/architecture.md`](docs/architecture.md) and
  [`docs/decisions.md`](docs/decisions.md).
- Run `just test-core` for a core change. If Flutter is available, run
  `just test-all`; do not report skipped tests as passing.
- Stored formats and the wire protocol follow
  [ADR 0029](docs/decisions.md#adr-0029). A change to anything stored needs a
  migration or a fixture update; never regenerate a released fixture set.
- Keep `core/` free of Flutter and platform policy.

Do not:

- add internal strategy, pricing, launch copy, run logs or personal data; those
  live in the project's private engineering archive;
- commit secrets, recovery phrases, keys, keystores, `.env` files or personal
  screenshots;
- open a public issue for a vulnerability — use the private advisory flow in
  [`SECURITY.md`](SECURITY.md).

A good change is focused, says what it changes and why, and reports the
commands it ran. AI-assisted work is welcome; read the diff and run the tests
yourself before submitting it.
