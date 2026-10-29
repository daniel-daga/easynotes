# Architecture decisions

This is the public index of the decisions that shape EasyNotes. The long-form
records are maintained in the project's private engineering archive; this page
records what was decided, so the code and the public docs have something to
point at.

Each entry has a stable anchor (`#adr-0001`, `#adr-0002`, and so on) used by
comments throughout the repository.

<a id="adr-0001"></a>
## ADR 0001 — CRDT: Yrs, with items in a map and fractional order keys

**Accepted.** Notes are Yrs documents. Items live in a map and are ordered by
fractional order keys, so two people editing the same list merge without a
central sequence. Yrs was chosen on runtime and bundle-size measurements over
Automerge.

<a id="adr-0002"></a>
## ADR 0002 — One Rust core everywhere, including the web

**Accepted.** `easynotes-core` holds crypto, the CRDT, storage and sync, and
compiles natively and to `wasm32-unknown-unknown`. There is no second
implementation for the browser. The Flutter UI is the only platform layer.

<a id="adr-0003"></a>
## ADR 0003 — Relay: Rust and axum, Postgres for the log

**Accepted.** The relay is a Rust service built on axum. Each note's log is an
append-only set of rows in Postgres. The relay shares the envelope parser with
the client and stores ciphertext, not note content.

<a id="adr-0004"></a>
## ADR 0004 — Sync: a derived outbox and an echoed acknowledgement

**Accepted, amended by ADR 0016 and ADR 0017.** The outbox is derived from the
local log rather than stored separately. The relay echoes what it has stored,
and settlement trims the outbox. Later decisions batch typing and multiplex
every note on one socket per device.

<a id="adr-0005"></a>
## ADR 0005 — The local store: SQLite with sealed values, not SQLCipher

**Accepted.** The local store is plain SQLite from the bundled amalgamation.
Every value that is not already an envelope is sealed under the master key with
the core's own XChaCha20-Poly1305. No second crypto library, no SQLCipher.

<a id="adr-0006"></a>
## ADR 0006 — Compaction: the log keeps what the relay has not acknowledged

**Accepted.** Compaction writes a snapshot and then replaces the log with the
envelopes the relay has not acknowledged. The snapshot is written first, so an
interrupted compaction leaves a readable log.

<a id="adr-0007"></a>
## ADR 0007 — The note list: opening every note, because the titles are ciphertext

**Accepted, amended by ADR 0009 and ADR 0017.** Listing notes originally meant
opening every note and summarising its document, because titles are encrypted.
ADR 0009 adds a sealed index so the list does not have to open every log.

<a id="adr-0008"></a>
## ADR 0008 — The account document: a document like any other, made of registers

**Accepted.** Personal state — placement, labels, settings, reminders — is a
CRDT under a reserved ID. It is sealed into the same envelopes, appended to the
same log and replayed like a note. Each field is a register, so two devices do
not overwrite unrelated settings.

<a id="adr-0009"></a>
## ADR 0009 — Local search, over a sealed index that the note list reads too

**Accepted.** A per-note sealed metadata row holds the title, item text, counts
and ordering. Search and the note list read that index instead of opening the
full note. The index is encrypted like the note.

<a id="adr-0010"></a>
## ADR 0010 — Forgetting a note: what a delete removes, and what it cannot

**Accepted.** Deleting removes the note's log, snapshot, metadata and key from
the local store. The relay's copy, collaborators' copies and backups are
separate concerns, addressed by ADR 0022 and ADR 0026.

<a id="adr-0011"></a>
## ADR 0011 — The store a browser gets, and the promise it cannot keep

**Accepted.** The browser store reads from memory and hands every mutation to
Dart as a batch of key-value changes. Writes are told, not awaited, because the
web platform cannot promise a synchronous flush of IndexedDB.

<a id="adr-0012"></a>
## ADR 0012 — Compacting the account document: the snapshot goes in the log

**Accepted.** The account document's snapshot is an envelope in its own log.
Compaction replaces the log with that envelope, and opening the account calls
it when the replayed log is longer than the snapshot warrants.

<a id="adr-0013"></a>
## ADR 0013 — The recovery phrase, and what it derives

**Accepted, amended by ADR 0023.** A person is a twelve-word BIP-39 phrase:
128 bits of entropy and a checksum. It derives the account root and the account
keys, but not note keys. It is generated once, shown once and never stored.

<a id="adr-0014"></a>
## ADR 0014 — What the relay learns about a person

**Accepted.** The relay does not learn that two devices belong to one person,
and it has no account identity. It sees opaque note IDs, device public keys,
sizes, timing, addresses and which device keys touch which notes.

<a id="adr-0015"></a>
## ADR 0015 — Invite links: a secret that asks, and an owner who answers

**Accepted, amended by ADR 0023, ADR 0025 and ADR 0030.** An invite is a random
secret carried in the URL fragment, so a browser never sends it to a server.
The owner seals a grant for that secret. Later decisions remove the approval
step and pre-seal the grant so the first person can join while the owner is
offline.

<a id="adr-0016"></a>
## ADR 0016 — Batching edits: typing waits for a pause, and goes as one envelope

**Accepted, amended 2026-10-26.** Typing is held until a pause and folded into
one envelope; a tick or other single gesture goes at once. The amendment sets a
calmer pace and says where a field takes a remote change.

<a id="adr-0017"></a>
## ADR 0017 — One socket per device

**Accepted, amends ADR 0004 and ADR 0007.** One authenticated WebSocket carries
every note, with the note named in each message. A session keeps a per-note
subscription map, and reading never waits for writing.

<a id="adr-0018"></a>
## ADR 0018 — Revocation is a creator-signed key generation

**Accepted, amended by ADR 0023.** Revocation is a creator-signed generation of
the note key. Remaining members receive the new key wrapped for their devices,
and the relay drops the removed key's subscription. The removed person keeps
what they already read.

<a id="adr-0019"></a>
## ADR 0019 — Reminders are account state mirrored to local schedulers

**Accepted.** A reminder is an encrypted account record with a random ID, the
note it belongs to, a UTC fire time, a repeat rule and an IANA time zone. Local
schedulers mirror the records; the relay sees none of the content.

<a id="adr-0020"></a>
## ADR 0020 — An encrypted collaborator directory

**Superseded by ADR 0023.** An earlier design that identified invited devices
and their chosen display names. It was replaced by account member keys and
access packages, where a collaborator is a person rather than a device.

<a id="adr-0021"></a>
## ADR 0021 — Stateless relay tokens

**Accepted.** Relay tokens are signed, versioned payloads containing an expiry,
the device's Ed25519 public key and a random nonce. Every instance can verify
one without a server-side session map, so restarts and extra instances do not
invalidate connected devices.

<a id="adr-0022"></a>
## ADR 0022 — Permanent deletion across one person's devices

**Accepted, amended by ADR 0026.** Permanent deletion writes a monotonic
`deleted:<note-id>` marker to the person's account document. It propagates to
their other devices and overrides note keys, placement and reminders.

<a id="adr-0023"></a>
## ADR 0023 — Share with people: account member keys and access packages

**Accepted, amended by ADR 0025.** Sharing is by account, not by device. The
recovery phrase derives account member keys, and an access package carries the
note key wrapped for the person's devices. A person is one collaborator however
many devices they use.

<a id="adr-0024"></a>
## ADR 0024 — A storage quota for an anonymous relay

**Accepted and built.** A relay enforces per-note and global byte ceilings.
Members compact a note's log to a base to stay under them, and any member may
compact, so a shared note does not depend on its owner being online.

<a id="adr-0025"></a>
## ADR 0025 — A link lets its first person in, once

**Accepted, amended by ADR 0030.** The relay takes one request per invite. The
first guest to present the secret claims it; every later request gets `410`.
There is no owner approval step and no safety code.

<a id="adr-0026"></a>
## ADR 0026 — Deleting an account, and what the relay forgets

**Accepted, amended by ADR 0034.** A signed purge request names one topic. The
relay deletes what the account owns and keeps a tombstone, so a deleted account
does not come back through a stale client. ADR 0034 adds the record a guest
leaves in notes they joined.

<a id="adr-0027"></a>
## ADR 0027 — Attachments: descriptors in the note, sealed chunks beside it

**Accepted and built.** Each attachment is a descriptor on the note. Its bytes
are sealed in chunks under a key derived for that attachment and transferred
over HTTP. The relay stores the chunks but cannot read them. Removed
attachments' bytes are reclaimed after a compaction no longer lists them.

<a id="adr-0028"></a>
## ADR 0028 — Who may write to a relay

**Accepted.** A relay access key is proven at the handshake, with a per-address
write budget as a second line. A relay open to strangers is a separate decision
and has not been taken. Self-hosted relays may use the same key gate.

<a id="adr-0029"></a>
## ADR 0029 — Compatibility across releases

**Accepted.** From the first release tag, stored formats stay readable or are
refused with an explicit protocol version. Migrations are tested against a
fixture set written by every earlier release. Once a set is released it is
never regenerated; a change gets a migration instead.

<a id="adr-0030"></a>
## ADR 0030 — A link lets its first person in, without the owner's phone

**Accepted.** The invite carries a pre-sealed grant. The first person to open
the link can join while the owner's device is offline, and the relay never
needs to ask the owner to approve.

<a id="adr-0031"></a>
## ADR 0031 — Sync is a server and the account that belongs to it

**Accepted, amends ADR 0008 and ADR 0013.** A profile has a sync server if and
only if it has an account. A local-only profile has neither and is not backed
up. "Backup" means the encrypted replica on the relay plus the recovery phrase
that makes it readable; the relay operator cannot restore it.

<a id="adr-0032"></a>
## ADR 0032 — A name that follows the person

**Accepted.** Each member signs their own display name into the note. A rename
reaches every note the person is on, in both directions, without letting one
member rewrite another's name.

<a id="adr-0034"></a>
## ADR 0034 — A guest leaves when their account is deleted

**Accepted.** Before deleting an account, the guest signs a "left" record into
every note they joined. The owner sees the record, removes the member and
rotates the note key on the next sync, so the deleted account does not linger
in other people's People lists.

<a id="adr-0036"></a>
## ADR 0036 — An item's text merges like a body

**Accepted.** A checklist item's text is the CRDT's own text type, as a note
body already was, so two people typing into one item at the same moment both
keep their words; a true collision shows them side by side rather than losing
one. Closing a note no longer deletes a blank item it made: the item is marked
dropped and hidden only while blank, so text a peer typed into it meanwhile
brings it back. A note that was just joined is not seeded with a blank item,
because its items are still arriving. Item text written by releases before
this is not read; those releases' documents keep everything else.
