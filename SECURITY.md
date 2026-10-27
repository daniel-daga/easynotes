# Security

EasyNotes is an end-to-end encrypted notes app. The claim it makes is that a
relay, or anyone who takes one over, learns what
[ADR 0014](docs/decisions.md#adr-0014) says it learns and nothing more, and
cannot change a note in a way its members would accept. A report that shows
otherwise is the most useful thing anyone can send this project.

## Status

**EasyNotes has not been externally audited.** An external review is planned
before 1.0. There has been one internal review of the cryptography; its three
findings are fixed and regression-tested. The attachment construction
([ADR 0027](docs/decisions.md#adr-0027)) came after that review and has not had
one of its own. Do not rely on EasyNotes for anything where a flaw would put
someone at risk until an audit report is published here.

No store release has been published yet. Only the latest commit on `main` is
supported, and a fix lands there.

## Reporting a vulnerability

Report privately, through GitHub: the repository's **Security** tab, then
**Report a vulnerability**. That opens a private advisory that only the
maintainers can see. Please do not open a public issue, pull request or
discussion for a vulnerability until a fix is out.

A useful report says what an attacker needs (a relay they run, a note key, an
invite link, a device), what they get, and how to reproduce it — a failing test
against `core/tests/` or `relay/tests/` is ideal.

What to expect:

- An acknowledgement within **5 working days**.
- An assessment — accepted, with a severity, or declined, with the reason —
  within **14 days**.
- For an accepted report, a fix, or a plan with a date, within **90 days**. You
  are credited in the advisory and the fix's commit unless you ask not to be.

This is a small project maintained by one person. If a deadline is going to
slip, you will be told before it does, and why.

## Scope

In scope:

- The Rust core (`core/`): encryption, signatures, key handling, invites,
  revocation, recovery phrases, the local stores, and the sync client.
- The relay (`relay/`): anything that lets a client read, alter, replay or
  delete another client's data, bypass authentication, or exhaust the relay in
  a way its quotas and rate limits ([ADR 0024](docs/decisions.md#adr-0024))
  do not already account for.
- The app (`app/`), where it leaks plaintext or keys — to logs, the clipboard,
  screenshots, other apps, or storage outside the sealed store.
- The Synology deployment (`compose.synology.yaml`, `deploy/synology/`), where
  following it as written leaves a relay exposed.

Known and documented, so not a vulnerability on its own:

- What the relay learns: note IDs, sizes, timing, the addresses devices connect
  from, and which device keys touch which notes ([ADR 0014](docs/decisions.md#adr-0014)).
- A relay can require an access key and budget writes per address
  ([ADR 0028](docs/decisions.md#adr-0028)); a relay open to strangers is not a
  supported mode yet.
- A person who has read a shared note keeps what they read after they are
  revoked; revocation protects what is written afterwards
  ([ADR 0018](docs/decisions.md#adr-0018)).
- Removed attachment bytes stay on the relay until a compaction stops listing
  them, and are then reclaimed about a week later
  ([ADR 0027](docs/decisions.md#adr-0027)).

A report that shows one of these is worse than documented is in scope.

Out of scope: attacks that need an unlocked device already in the attacker's
hands with the app open, social engineering of the maintainer, denial of
service by volume against someone else's relay, and findings from automated
scanners without a demonstrated impact.

## Safe harbour

Research done in good faith within this policy — on your own devices, your own
relay, and notes you own — is welcome, and will not be the subject of a
complaint from this project. Do not access other people's data, and do not test
against a relay you do not run without its operator's permission.
