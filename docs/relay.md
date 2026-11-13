# Running a relay

The EasyNotes relay stores and forwards encrypted updates and attachments.
It cannot read note content, but it sees routing and connection metadata; see
[SECURITY.md](../SECURITY.md). The app and relay are still experimental.

## Build and run

From the repository root, with stable Rust installed:

```sh
cargo build -p easynotes-relay --release
```

The binary is `target/release/easynotes-relay` (`.exe` on Windows). Running it
without configuration listens on `127.0.0.1:8080`, allows unrestricted access,
and keeps data in memory. **All in-memory data is lost on exit.** These defaults
are for development, not for an internet-facing relay.

For a persistent relay:

1. Create a Postgres 16 database and set `DATABASE_URL` to its connection URL.
   The relay runs its database migrations on connection.
2. Set `RELAY_MEDIA_DIR` to a writable directory on persistent storage. With
   Postgres but no media directory, attachments are disabled rather than stored
   in memory. Back up the database and media directory together.
3. Generate independent keys with `easynotes-relay access-key`. Set
   `RELAY_TOKEN_SIGNING_KEY` to one key and `RELAY_ACCESS_KEYS` to another.
   Retain them securely across restarts; do not commit them.
4. Put the relay behind an HTTPS reverse proxy that supports WebSocket upgrades.
   Restrict `RELAY_ALLOWED_ORIGINS` to your browser app's exact origins and
   configure `RELAY_TRUSTED_PROXIES` only for trusted proxy addresses.
5. Keep admin and metrics listeners off the public network. Test recovery from
   backups before relying on the deployment.

The root [Dockerfile](../Dockerfile) builds a non-root relay image. For a
complete container, reverse-proxy and backup example, use the
[Synology deployment guide](../deploy/synology/README.md). Its
[Compose file](../compose.synology.yaml) and [`.env.example`](../.env.example)
include deployment-specific settings; the standalone binary reads environment
variables, not `.env` files automatically.

## Configuration reference

Limits below take integer byte counts, not strings such as `1 GiB`. Defaults
describe the standalone binary; containers or Compose can override them.

| Variable | Default | Purpose |
| --- | --- | --- |
| `DATABASE_URL` | unset | Postgres connection URL; unset uses in-memory storage |
| `RELAY_ADDR` | `127.0.0.1:8080` | Listen address; the Docker image uses `0.0.0.0:8080` |
| `RELAY_ALLOWED_ORIGINS` | any | Comma-separated browser origins allowed to handshake; not a replacement for access control |
| `RELAY_TOKEN_SIGNING_KEY` | temporary process key | Base64url 32-byte Ed25519 seed for signed session tokens; keep stable across restarts and instances |
| `RELAY_TOKEN_TTL_SECS` | `900` | Positive session lifetime in seconds; bounds how long existing sockets can retain access after a token is revoked or made read-only |
| `RELAY_ACCESS_KEYS` | open unless token-only | Comma-separated base64url 32-byte access keys, or explicit `open`; use keys for restricted access |
| `RELAY_TOKENS` | off | `on` enables per-device access tokens; cannot be combined with `RELAY_ACCESS_KEYS=open` |
| `RELAY_ADMIN_ADDR` | unset | Optional private token-management listener, e.g. `127.0.0.1:9091` |
| `RELAY_ADMIN_SECRET` | unset | Base64url 32-byte bearer secret required with the admin listener; must differ from every access key |
| `RELAY_WRITE_BURST` | 200 MiB | Positive byte budget one client address may write at once |
| `RELAY_WRITE_PER_HOUR` | 200 MiB/hour | Positive sustained write budget per address; `off` disables the write budget |
| `RELAY_STORAGE_CEILING` | 16 GiB | Total live-log bytes before new notes are refused; existing notes can still append; `off` removes the ceiling |
| `RELAY_MEDIA_DIR` | unset | Persistent attachment directory; otherwise media is in-memory for an in-memory relay, disabled for Postgres |
| `RELAY_MEDIA_PER_NOTE` | 1 GiB | Attachment bytes per note, including upload reservations |
| `RELAY_MEDIA_CEILING` | 64 GiB | Total attachment byte ceiling; `off` removes it |
| `RELAY_MEDIA_STAGING_CEILING` | 8 GiB | Ceiling for upload reservations in progress; `off` removes it |
| `RELAY_TRUSTED_PROXIES` | unset | Proxy addresses/CIDRs trusted to supply `X-Forwarded-For` for rate limiting |
| `RELAY_RATE_LIMIT` | on | `off` disables rate limits, not storage/media ceilings; development only |
| `RELAY_DRAIN_SECONDS` | `8` | Graceful-shutdown timeout; keep below the container's stop grace period |
| `RELAY_METRICS_ADDR` | unset | Optional separate Prometheus `/metrics` listener |
| `RUST_LOG` | `easynotes_relay=info` | Rust tracing filter |

## Device access tokens

Shared access keys are enough for a small private relay. For individually
revocable device access, set `RELAY_TOKENS=on`. Leave `RELAY_ACCESS_KEYS` unset
for token-only access, or configure shared keys to accept either credential.
Do not use `open` with tokens; the relay refuses that combination.

Enable the private admin listener with `RELAY_ADMIN_ADDR` and an independent
`RELAY_ADMIN_SECRET`. The CLI uses those same environment variables to call
the running relay:

```sh
easynotes-relay token create
easynotes-relay token read-only <id> on
easynotes-relay token read-only <id> off
easynotes-relay token revoke <id>
easynotes-relay token unbind <id>
```

`create` prints `<id>.<secret>` for a relay link or QR code. Treat it as a
secret. A token binds to a device key on first use; `unbind` allows it to be
claimed again. Revocation and read-only changes reach already-open sockets
within `RELAY_TOKEN_TTL_SECS`.

The admin listener uses plain HTTP. Keep it on loopback, or behind a private
TLS-protected connection; otherwise bearer credentials and minted device
secrets cross the network in cleartext. It must never be a public route.

## Health, monitoring and operational limits

- `/v1/health` reports whether the process is up.
- `/v1/ready` also checks database availability. `easynotes-relay check` queries
  this endpoint at `RELAY_ADDR`, exiting 0 on success and 1 on failure.
- `/metrics` is available only on the optional `RELAY_METRICS_ADDR` listener.
  Example alerts are in [`deploy/prometheus/alerts.yml`](../deploy/prometheus/alerts.yml).

Account transfers use process-local pairings. One relay process must serve
both devices, and restarting it loses pending pairings. Shared database and
signing keys do not make transfers work across multiple instances.

Removed attachment bytes are reclaimed after compaction stops referencing them
and a retention delay; see [ADR 0027](decisions.md#adr-0027). Deletion does not
erase older encrypted backups or copies already held by collaborators.
