# EasyNotes

A Google Keep–style notes and lists app where every note is end-to-end
encrypted, lists can be edited live by several people, and sharing needs no
email account. See [`docs/design.md`](docs/design.md) for the product and
architecture.

**Status: Slice 1 complete, Slice 2 in progress.** The vertical spike works —
one Rust core for crypto, CRDT, storage and sync, driving a Flutter UI on Linux
desktop and in a browser, with a relay that stores and forwards ciphertext and
nothing else. [`docs/slice-01-results.md`](docs/slice-01-results.md) is the
honest account of that, including what it did not do;
[`docs/plan-slice-02.md`](docs/plan-slice-02.md) is what happens next and how
far it has got.

The app now opens on a grid of lists rather than on the single note Slice 1
had: make one, join someone else's and it is added rather than swapped in, and
every one of them syncs. Hold a card and it offers the palette, a pin, the
archive and the trash — all of them *personal*, in a private account document of
this device's own, so colouring a shared shopping list does not recolour it for
whoever else is on it. Edits go to a relay as they are made, an edit made with
no network waits in the local log and goes when there is one, and the app says
which of the two it is. It keeps them in SQLite, with the note keys and the
device identity wrapped by a master key rather than sitting in files beside the
log, and a device that has a Slice 1 directory is carried into the database the
first time it opens. A log that has grown past a few hundred edits is compacted
to a snapshot when the note is opened, minus whatever the relay has not
acknowledged yet — which is the only copy of those edits anywhere.

Listing notes means opening them, because a title is ciphertext until a key
opens it. `just bench-library` says what that costs: a household's ten lists open
in a couple of milliseconds, two hundred take most of a second, and
[ADR 0007](docs/adr/0007-note-list.md) says what to do about it when someone has
two hundred.

What the account document does not do yet is sync: a colour stays on the device
that chose it, because there is no such thing as one person's two devices until
accounts land. Nor is there a way to delete a note for good — the trash keeps it,
out of sight and still on the disk. Tags, reminders and search are the rest of
that document's remit and are not built.

Android and iOS have still never been built — that is the largest open gap and
the next thing to do on a machine that has an Android SDK and a Mac.

## Layout

```
core/    Rust: crypto, CRDT, local store, sync client. No Flutter, no platform policy.
relay/   Rust (axum): WebSocket relay over Postgres. Ciphertext only.
app/     Flutter app, plus the thin bridge crate in app/rust.
docs/    Design, plans, and the decision records.
tools/   Measurement helpers that are not part of the product.
```

`core` stays callable from a plain `cargo test` with no Flutter in the loop.
That is what keeps the security-critical code auditable, and it is not
negotiable.

## Getting started

Needs a stable Rust toolchain, Flutter, and [`just`](https://github.com/casey/just).

```sh
just test-core        # the core and everything under it, no Flutter needed
just test-all         # core, relay and app
just run-relay        # a relay on 127.0.0.1:8080, storing in memory
just run-linux        # the app on Linux desktop, over the real core
just --list           # everything else
```

The two-device demo is `just run-relay` in one terminal and `just run-linux` in
another — twice, if you want two devices. The app syncs with the relay at
`EASYNOTES_RELAY`, which defaults to the one `just run-relay` starts:

```sh
just run-linux https://relay.example     # or wherever yours is
```

With no relay there, nothing breaks: the app is a local notes app and the status
button in the app bar says so, with the count of edits that have not left the
device. To share a list, open it and press the share button, then paste the blob
into the join button on the note list of the other device. It carries the note's
key, so it is as sensitive as the list itself.

The relay's Postgres tests skip unless `DATABASE_URL` is set. CI sets it, and
they are the only cover for the concurrency in sequence allocation — so a green
run without it is a run that skipped the tests that matter there.

The web build needs a nightly toolchain and a few extras; `just setup-web`
installs them, then `just build-web`. Whatever serves the result must send
`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`, because the WebAssembly is
threaded and will not start without cross-origin isolation.

## Decisions

| | |
| --- | --- |
| [ADR 0001](docs/adr/0001-crdt.md) | CRDT: Yrs, with items in a map and fractional order keys |
| [ADR 0002](docs/adr/0002-web-core.md) | One Rust core everywhere, including the web |
| [ADR 0003](docs/adr/0003-relay.md) | Relay: Rust and axum, Postgres for the log |
| [ADR 0004](docs/adr/0004-sync-engine.md) | Sync: an outbox derived from the log, acknowledged by echo |
| [ADR 0005](docs/adr/0005-local-store.md) | Local store: SQLite with sealed values, not SQLCipher |
| [ADR 0006](docs/adr/0006-compaction.md) | Compaction: the log keeps what the relay has not acknowledged |
| [ADR 0007](docs/adr/0007-note-list.md) | The note list: listing notes means opening them |
| [ADR 0008](docs/adr/0008-account-document.md) | The account document: a document like any other, made of registers |

## Security, in one paragraph

Each note has its own key, and a master key — kept in the OS keychain — wraps
those and the device seed on this device. Every update is encrypted with XChaCha20-Poly1305 and
signed with the author's Ed25519 device key; a receiver **verifies the signature
before it decrypts**, and the AAD binds the note, the author and a counter, so an
envelope cannot be moved between notes or re-attributed. The relay holds no keys.
It may check a token, a size, and that an envelope's note ID matches the path it
arrived on — it may not check anything else, and it never sees plaintext.
`the_relay_never_sees_plaintext` enforces that against the actual bytes on the
wire rather than trusting the claim, and the same check runs against the local
database and its write-ahead log. The sync engine that moves those envelopes
holds no keys and cannot read one: it hands every arrival to the core, which
verifies before it decrypts.

This has **not** been audited. The design doc schedules an external review before
1.0, and nothing here should be relied on until then.
