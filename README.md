# EasyNotes

A Google Keep–style notes and lists app where every note is end-to-end
encrypted, lists can be edited live by several people, and sharing needs no
email account. See [`docs/design.md`](docs/design.md) for the product and
architecture.

**Status: not production-ready ([2026-09-30 review](docs/production-readiness-2026-09-30.md)).
Slices 1 and 2 are complete, and attachments ([ADR 0027](docs/adr/0027-attachments.md))
work on native and on the web. Android has been built and exercised on a Pixel;
a signed release has not yet been cut through `release.yml`, iOS still needs a
Mac, and the full hardware demo remains open.** The vertical spike works —
one Rust core for crypto, CRDT, storage and sync, driving a Flutter UI on Linux
desktop and in a browser, with a relay that stores and forwards ciphertext and
nothing else. [`docs/slice-02-results.md`](docs/slice-02-results.md) is the honest
account of where that got to, including what it did not do. The remaining Slice
3 work is recorded in [`docs/plan-slice-03.md`](docs/plan-slice-03.md): finish
the platform and hardware checks, including iOS and the Pixel invite flow.

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
the file to check. The deletion marker syncs through this person's encrypted
account document, so their other devices purge their copies when they sync. It
cannot erase relay history, backups or a guest's copy; the confirmation says
that clearly.

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
invite link that carries one secret and no note key. Whoever opens it first
joins under their name, and the owner's device lets them in without anyone
tapping Approve; the key is handed over sealed to that one person. A link works
once, for one account — anybody else who opens it is told it has been used —
and stops after seven days. The relay that holds the invite in between cannot
read who joined or which note it is for, and is never told which devices are on
either side. [ADR 0015](docs/adr/0015-invites.md) and [ADR
0025](docs/adr/0025-links-that-let-people-in.md) have the decisions and what
they cost: a leaked link lets its finder in until the owner removes them, the
owner's device has to be reachable to let anyone in, both sides poll rather
than being woken, and the link is pasted rather than tapped until the platforms
register it.

**A note is shared with people, not phones.** A collaborator is an account under
the name its owner chose, so Anna on two phones is one entry, and a phone she
restores from her twelve words gets back every note shared with her without
asking anyone. The owner can remove one person, and everyone else keeps the note
and does nothing. Every change to who can read a note is an *access package*: one
envelope, signed by the note's owner key, with the whole list, a key for each
person on it and, when anyone leaves, a fresh key with the note sealed under it.
Every device works out which package is current from the packages alone, so the
relay still checks nothing and learns no names, member IDs or head count. [ADR
0023](docs/adr/0023-access-packages.md) has the format and what Remove can and
cannot promise; [the plan](docs/plan-account-collaboration.md) has the flows.

What is built is the core of it, with the property the whole item exists for held
against both CRDT backends: two devices adopted from one phrase agree about a
note's contents *and* about its placement. What is not built yet is the wiring —
nothing subscribes to the account document over a relay, and the app has no
screen for any of it, which includes the forced backup check that makes somebody
write the phrase down while it still exists. Reminders now complete the first
local account-document feature beyond placement: they sync as private account
state and schedule locally, with push wake-ups deliberately deferred.

Android now builds and runs on a Pixel. Real-device Wi-Fi sync and local data
surviving an offline force-stop/relaunch have been verified; the invite approval
flow still needs a successful Pixel check, and iOS needs a Mac. See the current
hardware notes in [`docs/pixel-demo-2026-09-23.md`](docs/pixel-demo-2026-09-23.md)
and the remaining work in [`docs/plan-slice-03.md`](docs/plan-slice-03.md).

Portable text sharing is available: text notes share as plain text, checklists
share as Markdown task lists, and pasting task-list Markdown into a text note
then switching to checklist mode keeps checked items. The other direction works
too: text shared *into* EasyNotes from another app's share sheet (Android, or
an installed web app through the Web Share Target) becomes a new note, and a
checklist shared out and back in comes back as a checklist. The details are in
[`docs/plan-text-interchange.md`](docs/plan-text-interchange.md); the
Google Keep Takeout importer is deferred.

A note can hold photos and sound files ([ADR
0027](docs/adr/0027-attachments.md), [the plan](docs/plan-attachments.md)).
Each file is encrypted in chunks under a key of its own. That key is carried in
the note's encrypted updates, like its text, and the bytes travel to the relay
separately, over HTTP. A file reaches anyone else on the note only after the
relay holds all of it, and an import made offline waits for the relay. The
same transfer queue runs on a device, over a directory, and in a tab, over an
IndexedDB database of its own.

The editor has **Add attachment**, a picture gallery above the title with a
full-screen viewer, and rows for sound files. Upload and download states show
on each item, and a card shows the note's first picture. Not built yet: an
audio player, the camera, and paste and drop. A removed attachment's bytes
leave the relay about a week after the compaction its removal asks for
([ADR 0027](docs/adr/0027-attachments.md) §5).

`just web-shared-text` verifies the browser's incoming share route: text and
checklists keep their content and checked state after reload, and a consumed
share URL does not create a duplicate note. It needs a web build, but no relay.

## Layout

```
core/    Rust: crypto, CRDT, local store, sync client. No Flutter, no platform policy.
relay/   Rust (axum): WebSocket relay over Postgres. Ciphertext only.
app/     Flutter app, plus the thin bridge crate in app/rust.
compat/  What each release wrote, read back by every later build's tests (ADR 0029).
docs/    Design, plans, and the decision records.
tools/   Measurement helpers and build checks that are not part of the product.
```

`core` stays callable from a plain `cargo test` with no Flutter in the loop.
That is what keeps the security-critical code auditable, and it is not
negotiable.

## Getting started

Windows desktop build instructions and validation limits are in
[`docs/windows-desktop.md`](docs/windows-desktop.md). `just build-windows`
produces an unpackaged x64 app; `just run-windows` runs a development build.

Needs a stable Rust toolchain, Flutter, and [`just`](https://github.com/casey/just).

```sh
just test-core        # the core and everything under it, no Flutter needed
just test-all         # core, relay and app
just run-relay        # a relay on 127.0.0.1:8080, storing in memory
just run-linux        # the app on Linux desktop, over the real core
just --list           # everything else
```

The two-device demo is `just run-relay` in one terminal and `just run-linux` in
another — twice, if you want two devices. In each app, open the drawer’s
**Sync & devices → Set up sync** and enter `http://127.0.0.1:8080` (or the URL of your relay).
The URL is saved in the app, so release builds can choose a relay after
installation.

On a new install, notes start in **On this device** until a relay is selected.
Setup offers to move the current local collection to the new relay or start
with an empty collection and keep the local notes separate. Moved notes remain
available offline. Each relay has its own notes and account; switching relays
leaves both collections available. Relay sync uploads encrypted updates even
without an account. An account and its recovery phrase let another device find
and unlock the same notes, using the same relay. To share a list, open it, press the share
button and send the link it makes. The link carries no key: the first person to
open it joins, and nobody after them can ([ADR 0015](docs/adr/0015-invites.md),
[ADR 0023](docs/adr/0023-access-packages.md),
[ADR 0025](docs/adr/0025-links-that-let-people-in.md)).

The relay's Postgres tests skip unless `DATABASE_URL` is set. CI sets it, and
they are the only cover for the concurrency in sequence allocation — so a green
run without it is a run that skipped the tests that matter there.

Production relays should set `RELAY_TOKEN_SIGNING_KEY` to a stable base64url
encoding of 32 random bytes. The same value must be shared by every relay
instance and kept across restarts so signed device tokens remain valid; the
Synology setup documents key generation, storage and rotation. Without it, the relay uses
a temporary process key intended only for local development.

A relay lets in only devices holding one of its access keys when
`RELAY_ACCESS_KEYS` names any ([ADR 0028](docs/adr/0028-who-may-write-to-a-relay.md)).
Its operator hands out a relay link, `https://relay.example/#access=<key>`, which
goes in Settings → Relay. Set it to `open` to let anyone write; unset is open,
with a warning, and the Synology setup refuses to start without it.
`easynotes-relay access-key` prints a new key. Every relay also holds each client
address to a write budget, 200 MiB at once and 200 MiB an hour by default
(`RELAY_WRITE_BURST`, `RELAY_WRITE_PER_HOUR`).

A relay stops taking *new* notes once every note together holds 16 GiB. Notes it
already has keep working. `RELAY_STORAGE_CEILING` sets that ceiling in bytes, or
`off` removes it. Each note's live log is capped at 32 MiB. Members keep far
below that by compacting it when the relay asks
([ADR 0024](docs/adr/0024-relay-storage-quota.md)).

Attachment bytes live apart from the log, in `RELAY_MEDIA_DIR`, and are backed
up with the database. Without it a relay on Postgres turns attachments off, and
one in memory keeps them in memory. `RELAY_MEDIA_PER_NOTE` (1 GiB) and
`RELAY_MEDIA_CEILING` (64 GiB, or `off`) bound them
([ADR 0027](docs/adr/0027-attachments.md) §4).

The relay rate-limits its unauthenticated doors per client address. Behind a
reverse proxy every client has the proxy's address, so name the proxy in
`RELAY_TRUSTED_PROXIES` (addresses or CIDR ranges, comma separated): from those
peers, and only those, the relay reads the client from `X-Forwarded-For`.

On `SIGTERM` or Ctrl-C the relay stops taking connections, closes every sync
socket cleanly so devices reconnect on their own, and gives requests in flight
`RELAY_DRAIN_SECONDS` (8) to finish. `easynotes-relay check` asks a running
relay's `/v1/ready` and exits 0 or 1, for a container health check.

`RELAY_METRICS_ADDR` (for example `0.0.0.0:9090`) serves Prometheus metrics at
`/metrics` on a listener of its own, kept off the public one: storage and
attachment bytes against their ceilings, open sockets, rate-limit and quota
refusals, and responses by status class. Example alerts, at 80% of each
ceiling among others, are in
[`deploy/prometheus/alerts.yml`](deploy/prometheus/alerts.yml).

For a small private deployment on a Synology NAS, see
[`deploy/synology/README.md`](deploy/synology/README.md). It builds the relay
container, runs Postgres with persistent storage, and uses DSM's reverse proxy
for HTTPS and WebSockets.

The web build needs a nightly toolchain and a few extras; `just setup-web`
installs them, then `just build-web`. Four recipes then run the result in a
headless browser: `just web-smoke` checks that what a tab writes to IndexedDB is
there when it is reloaded, `just web-sync-smoke` starts a relay and checks
that a tab reaches it, publishes, and is acknowledged, and `just web-converge`
has two tabs share a list through an invite link and edit it, online and
offline, until they agree, then switch it to text and back. `just
web-storage-failure` makes IndexedDB itself refuse each write of a deletion in
turn and checks that no note is ever left half deleted. Whatever serves the result must send
`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`, because the WebAssembly is
threaded and will not start without cross-origin isolation.

`just bench-web` is the fifth, and it needs none of that build: it compiles the
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
| [ADR 0015](docs/adr/0015-invites.md) | Invite links: a secret that asks, and an owner who answers (*amended by 0025:* nobody approves) |
| [ADR 0016](docs/adr/0016-batching-edits.md) | Batching edits: typing waits for a pause, and goes as one envelope |
| [ADR 0017](docs/adr/0017-one-socket-per-device.md) | One socket per device: every note on it, and reading never waits for writing |
| [ADR 0018](docs/adr/0018-revocation.md) | Revocation is a creator-signed key generation |
| [ADR 0019](docs/adr/0019-reminders.md) | Reminders are account state mirrored to local schedulers |
| [ADR 0020](docs/adr/0020-collaborator-directory.md) | *Superseded by 0023:* an encrypted collaborator directory |
| [ADR 0021](docs/adr/0021-stateless-relay-tokens.md) | Stateless relay tokens: signed expiry and shared one-use challenges |
| [ADR 0022](docs/adr/0022-account-wide-deletion.md) | Permanent deletion across one person's devices |
| [ADR 0023](docs/adr/0023-access-packages.md) | Share with people: account member keys and access packages |
| [ADR 0024](docs/adr/0024-relay-storage-quota.md) | A storage quota for an anonymous relay: members compact a note's log to a base |
| [ADR 0025](docs/adr/0025-links-that-let-people-in.md) | A link lets its first person in, once: no approval, no safety code |
| [ADR 0026](docs/adr/0026-deleting-an-account.md) | Deleting an account: the relay purges what the person owns and keeps a tombstone |
| [ADR 0027](docs/adr/0027-attachments.md) | Attachments: descriptors in the note, media in sealed chunks beside it (§1–§4 accepted and built, with the first UI; §5, relay clean-up, proposed) |
| [ADR 0028](docs/adr/0028-who-may-write-to-a-relay.md) | Who may write to a relay: a shared access key proven at the handshake, a per-address write budget, no relay for strangers yet |
| [ADR 0029](docs/adr/0029-compatibility-across-releases.md) | Compatibility across releases: what stays readable, a refusable protocol version, migrations tested from every release's fixtures (accepted and built; the old-relay-against-new-client job follows the first tag) |

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
is not claimed to be more; account devices purge their copies when the deletion
marker syncs, and a note shared with someone else remains readable to them if
they already copied or decrypted it, but
future access can be revoked by rotating the note key and replacing the history
with an owner-authorized snapshot. The sync engine that moves those envelopes
holds no keys and cannot read one: it hands every arrival to the core, which
verifies before it decrypts.

This has **not** been audited. The design doc schedules an external review before
1.0, and nothing here should be relied on until then. To report a
vulnerability, see [`SECURITY.md`](SECURITY.md).

