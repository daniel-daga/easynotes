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
whoever else is on it. Keep holding and the card comes up with your finger:
drop it on another and it takes that one's place, which is the same document
and the same privacy — where a list sits in your grid is yours, not the
household's. Edits go to a relay as they are made, an edit made with
no network waits in the local log and goes when there is one, and the app says
which of the two it is. It keeps them in SQLite, with the note keys and the
device identity wrapped by a master key rather than sitting in files beside the
log, and a device that has a Slice 1 directory is carried into the database the
first time it opens. In a browser the same rows, sealed the same way, go into
IndexedDB — so a tab that is closed and opened again has the list it had, which
until now it did not. [ADR 0011](docs/adr/0011-the-web-store.md) says what it
took to fit an asynchronous database behind a synchronous trait, and the one
promise a browser makes more weakly than a phone: an edit is kept shortly after
it is made rather than by the time it returns. A tab going away empties the
queue rather than waiting for the next tick, which narrows that window without
closing it — and on the rare occasion a browser refuses to store an edit at all,
out of quota or with its database cleared underneath the tab, the app says so
across the top of whichever screen is open instead of carrying on looking saved. A log that has grown past a few hundred edits is compacted
to a snapshot when the note is opened, minus whatever the relay has not
acknowledged yet — which is the only copy of those edits anywhere.

There is a search box, and it reads a sealed index of every note's title and
text rather than a network: type a word and the lists holding it come back, with
the line that matched on them, in an aeroplane. That index is also what the grid
is drawn from, so listing notes no longer means opening them — a household's ten
lists appear in under a millisecond and two hundred in nine, where two hundred
used to take most of a second. [ADR 0009](docs/adr/0009-search-index.md) says how
it stays honest about a note it has not opened, and what is now the slowest thing
a launch does.

A note in the trash can be deleted for good, one at a time or all at once, and
that means gone: the log, the snapshot, the search entry that held its text, the
key that opened it and this person's placement of it. [ADR
0010](docs/adr/0010-forgetting-a-note.md) says in what order, and why a `DELETE`
alone was not enough — it leaves the bytes in the file, which a test now reads
the file to check. What it cannot do is reach the relay's copy or a guest's: the
dialog that deletes a list says so, because that is the promise the button would
otherwise be making.

A browser is a device like any other now: it keeps its notes and it syncs them,
over the WebSocket the tab lends it rather than a TCP socket it cannot open. The
engine above the socket is the same code either way — only the transport differs
— and the relay learned to answer the handshake for a page, which it could not
before.

Notes can be labelled, and a label is a shelf: hold a card, type a name, and the
drawer grows an entry with that label's notes on it — with a search made there
searching only them, and a note made there getting the label rather than
appearing and vanishing. Labels are personal like the colours, for the same
reason and in the same document, and a label is nothing but the notes that carry
it: take it off the last one and it is gone. [ADR
0008](docs/adr/0008-account-document.md) says why each label is a key of its own
rather than a list in one.

What the account document does not do yet is sync: a colour stays on the device
that chose it, because there is no such thing as one person's two devices until
accounts land. Reminders are the rest of that document's remit and are not built,
and a label cannot yet be renamed.

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
device. The web build says the same thing about the same relay — it reaches one
through the browser's own WebSocket. To share a list, open it and press the share button, then paste the blob
into the join button on the note list of the other device. It carries the note's
key, so it is as sensitive as the list itself.

The relay's Postgres tests skip unless `DATABASE_URL` is set. CI sets it, and
they are the only cover for the concurrency in sequence allocation — so a green
run without it is a run that skipped the tests that matter there.

The web build needs a nightly toolchain and a few extras; `just setup-web`
installs them, then `just build-web`. Two recipes then run the result in a
headless browser: `just web-smoke` checks that what a tab writes to IndexedDB is
there when it is reloaded, and `just web-sync-smoke` starts a relay and checks
that a tab reaches it, publishes, and is acknowledged. Whatever serves the result must send
`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`, because the WebAssembly is
threaded and will not start without cross-origin isolation.

`just bench-web` is the third, and it needs none of that build: it compiles the
core alone for WebAssembly and measures what opening a device costs a tab —
reading the database, replaying the account document, and unsealing one index
entry a note. Two hundred notes is about 148 ms, five sixths of it IndexedDB.
[ADR 0011](docs/adr/0011-the-web-store.md) has the table and what it means for
reading the whole database at open.

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
| [ADR 0009](docs/adr/0009-search-index.md) | Local search, over a sealed index that the note list reads too |
| [ADR 0010](docs/adr/0010-forgetting-a-note.md) | Forgetting a note: what a delete removes, and what it cannot |
| [ADR 0011](docs/adr/0011-the-web-store.md) | The store a browser gets: reads from memory, writes through Dart |
| [ADR 0012](docs/adr/0012-compacting-the-account-document.md) | Compacting the account document: the snapshot goes in the log |

## Security, in one paragraph

Each note has its own key, and a master key — kept in the OS keychain, or in a
browser in the nearest thing one has — wraps those and the device seed on this
device. Every update is encrypted with XChaCha20-Poly1305 and
signed with the author's Ed25519 device key; a receiver **verifies the signature
before it decrypts**, and the AAD binds the note, the author and a counter, so an
envelope cannot be moved between notes or re-attributed. The relay holds no keys.
It may check a token, a size, and that an envelope's note ID matches the path it
arrived on — it may not check anything else, and it never sees plaintext. Its
handshake answers any origin, so that a page can do it at all; that costs
nothing, because a token needs a signature over the relay's nonce and a hostile
page has no device key to make one with.
`the_relay_never_sees_plaintext` enforces that against the actual bytes on the
wire rather than trusting the claim, and the same check runs against the local
database and its write-ahead log. The one thing on this device that holds a note
in a form the CRDT did not produce — the search index — is sealed under that
note's own key, so it cannot be read by anything that could not already read the
note, and cannot outlive access to it. Deleting a note removes all of that and
overwrites it rather than unlinking it, which
`a_forgotten_note_is_not_in_the_database_or_its_wal` checks against the actual
bytes of both databases; in a browser it is one range delete over the rows a
note has, and what a browser does with the freed space is the browser's, which
[ADR 0011](docs/adr/0011-the-web-store.md) says along with the rest of what the
web is weaker at —  the master key there is obfuscation over `localStorage` and
is not claimed to be more; it is this device's copy and no other, and a note that
was shared stays with whoever holds its key until Slice 3 brings revocation. The sync engine that moves those envelopes
holds no keys and cannot read one: it hands every arrival to the core, which
verifies before it decrypts.

This has **not** been audited. The design doc schedules an external review before
1.0, and nothing here should be relied on until then.
