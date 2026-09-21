# EasyNotes

A Google Keep–style notes and lists app where every note is end-to-end
encrypted, lists can be edited live by several people, and sharing needs no
email account. See [`docs/design.md`](docs/design.md) for the product and
architecture.

**Status: Slices 1 and 2 complete, except for the Android and iOS builds.** The vertical spike works —
one Rust core for crypto, CRDT, storage and sync, driving a Flutter UI on Linux
desktop and in a browser, with a relay that stores and forwards ciphertext and
nothing else. [`docs/slice-02-results.md`](docs/slice-02-results.md) is the honest account of
where that got to, including what it did not do, and
[`docs/plan-slice-03.md`](docs/plan-slice-03.md) is what happens next: the
mobile builds, and then accounts, so that one person can have two devices.

The app now opens on a grid of lists rather than on the single note Slice 1
had: make one, join someone else's and it is added rather than swapped in, and
every one of them syncs. Hold a card and it offers the palette, a pin, the
archive and the trash — all of them *personal*, in a private account document of
this device's own, so colouring a shared shopping list does not recolour it for
whoever else is on it. Keep holding and the card comes up with your finger:
drop it on another and it takes that one's place, which is the same document
and the same privacy — where a list sits in your grid is yours, not the
household's. Edits go to a relay as they are made — typing once it pauses, as one
envelope rather than one per keystroke ([ADR 0016](docs/adr/0016-batching-edits.md)),
and every note over one socket per device rather than one each ([ADR 0017](docs/adr/0017-one-socket-per-device.md)) —
an edit made with no network waits in the local log and goes when there is one, and the app says
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
reason and in the same document.

A label is a thing rather than a word: it has an identity of its own and its name
hangs off it, so it can be **renamed** — every note keeps it, because nothing
that records a membership has ever mentioned the name — and it outlives the notes
that carried it, so taking it off the last one leaves an empty shelf rather than
deleting it behind your back. Renaming a label onto a name another already has
merges the two, which is also the repair for the one duplicate two offline
devices can make. Throwing a label away is a separate gesture from emptying it,
and it says how many notes it is about to come off. [ADR
0008](docs/adr/0008-account-document.md) says why the name is a register rather
than the key, and what the shape before it could not do.

**There is such a thing as one person's two devices now.** An account is twelve
words — a BIP-39 recovery phrase, generated once and shown once — and everything
it derives is the account document's key and the ID it syncs under. A second
device that can say the words arrives at the same document, and therefore at the
same colours, labels, pins and placements, and at the note keys it needs to
*open* the notes rather than only store them. The relay is told none of this: two
devices of one person are two unrelated device keys to it, indistinguishable from
two people sharing a note. [ADR 0013](docs/adr/0013-recovery-phrase.md) says what
the phrase derives and — just as much the point — what it deliberately does not,
and [ADR 0014](docs/adr/0014-what-the-relay-learns.md) says what the relay is not
told and what that costs.

**A list is shared by a link now, and the link is not the key.** Share makes an
invite link that carries one secret and no note key. Whoever opens it asks to
join under a name; the owner sees the request with six digits the guest's screen
shows too, and lets them in or turns them away. Only then is the key handed over,
sealed to that one guest. A link admits one person and stops after seven days,
and the relay that holds the invite in between cannot read who asked or which
note it is for, and is never told which devices are on either side. [ADR
0015](docs/adr/0015-invites.md) has the decisions and what they cost: the
owner's device has to be reachable to let anyone in, both sides poll rather than
being woken, and the link is pasted rather than tapped until the platforms
register it.

What is built is the core of it, with the property the whole item exists for held
against both CRDT backends: two devices adopted from one phrase agree about a
note's contents *and* about its placement. What is not built yet is the wiring —
nothing subscribes to the account document over a relay, and the app has no
screen for any of it, which includes the forced backup check that makes somebody
write the phrase down while it still exists. Reminders are the rest of that
document's remit and are not built.

Android and iOS have still never been built — that is the largest open gap, two
slices old now, and the next thing to do on a machine that has an Android SDK
and a Mac.

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
| [ADR 0013](docs/adr/0013-recovery-phrase.md) | The recovery phrase: twelve words deriving one document, and not the note keys |
| [ADR 0014](docs/adr/0014-what-the-relay-learns.md) | What the relay learns about a person: nothing new |
| [ADR 0015](docs/adr/0015-invites.md) | Invite links: a secret that asks, and an owner who answers |
| [ADR 0016](docs/adr/0016-batching-edits.md) | Batching edits: typing waits for a pause, and goes as one envelope |
| [ADR 0017](docs/adr/0017-one-socket-per-device.md) | One socket per device: every note on it, and reading never waits for writing |

## Security, in one paragraph

Each note has its own key, and a master key — kept in the OS keychain, or in a
browser in the nearest thing one has — wraps those, the device seed, and the
account root on this device. The account root is what a recovery phrase derives;
the phrase itself is never stored anywhere, which is why losing it cannot be
recovered from and why the relay cannot help. Note keys are **not** derived from
it — they stay random and ride inside the account document's ciphertext, so that
a key can be rotated later, and so that a key somebody else shared with you has
somewhere to live. Every update is encrypted with XChaCha20-Poly1305 and
signed with the author's Ed25519 device key; a receiver **verifies the signature
before it decrypts**, and the AAD binds the note, the author and a counter, so an
envelope cannot be moved between notes or re-attributed. The relay holds no keys.
It may check a token, a size, and that an envelope's note ID matches the note it
was sent for — it may not check anything else, and it never sees plaintext. Its
handshake answers any origin by default, so that a page can do it at all; that
costs nothing, because a token needs a signature over the relay's nonce and a
hostile page has no device key to make one with — and a deployment that knows
which page it serves can name it in `RELAY_ALLOWED_ORIGINS` anyway. What a
public relay did need is a limit on how often a stranger may knock: the
handshake and opening a socket are a token bucket per address now, on by
default and off in every test.
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
