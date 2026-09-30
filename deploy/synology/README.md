# Deploy the relay on Synology

This runs the relay and Postgres in Synology Container Manager. The relay
container listens on its Docker network; only the NAS itself can reach it at
`127.0.0.1:18080`. DSM's reverse proxy provides the public HTTPS endpoint and
WebSocket upgrade.

## 1. Prepare the project on the NAS

Install **Container Manager** from Package Center. Copy or clone the complete
EasyNotes repository to a NAS folder such as `/volume1/docker/easynotes`. The
Compose file and Dockerfile use the repository root as their build context, so
keep the repository structure intact.

In that folder, copy `.env.example` to `.env`. Edit `.env`:

- Set `POSTGRES_PASSWORD` to a long random hexadecimal secret. For example,
  generate one on a computer with `openssl rand -hex 32`.
- Set `RELAY_ALLOWED_ORIGINS` to the exact origin serving the web app, such as
  `https://notes.example.net`. Use a comma-separated list if the app is served
  from more than one origin. Native apps do not send browser origins; if you
  only use native apps, set it to the relay's own HTTPS origin.
- Set `RELAY_TOKEN_SIGNING_KEY` to a stable random 32-byte key encoded as
  base64url. Generate it once with `openssl rand -base64 32 | tr '+/' '-_' |
  tr -d '=\n'`, store it in `.env`, and keep a protected backup. Every relay
  instance must use the same key. Changing it immediately invalidates active
  device tokens; retaining it lets them remain usable until expiry.
- Optionally set `RELAY_STORAGE_CEILING` to the number of bytes the relay
  may hold before it refuses to start new notes. Notes it already has keep
  working. Leave it empty for the default of 16 GiB, or set it to `off`.
- Optionally set `RELAY_MEDIA_PER_NOTE` and `RELAY_MEDIA_CEILING`, the bytes
  one note's attachments may hold (default 1 GiB) and all of them together
  (default 64 GiB, or `off`). Removing an attachment does not yet free space
  on the relay ([ADR 0027](../../docs/adr/0027-attachments.md) §5), so a note
  whose attachments change often reaches its budget sooner than its live
  files suggest.
- Leave `RELAY_NETWORK` empty unless the NAS already uses `172.30.87.0/24`.
  It is the subnet of the Compose network, and the relay trusts the
  `X-Forwarded-For` header only from that range, which is where DSM's reverse
  proxy reaches it from. Without that trust every device would arrive from one
  address and share one rate limit. If you change it, pick another private
  range that nothing else on the NAS uses.
- Set `NAS_DATA_DIR` to a persistent shared-folder path, for example
  `/volume1/docker/easynotes-data`.

Create the data directory in File Station and ensure the Container Manager
service can write to it. Postgres stores its database under
`$NAS_DATA_DIR/postgres`, and the relay keeps attachment bytes under
`$NAS_DATA_DIR/media`. The relay runs as uid 10001, so create that folder
and hand it over before the first start, over SSH:

```sh
mkdir -p /volume1/docker/easynotes-data/media
sudo chown 10001:10001 /volume1/docker/easynotes-data/media
```

A relay that cannot write there refuses to start and says so in its log.

## 2. Build and start the project

In **Container Manager → Project → Create**:

1. Name it `easynotes-relay`.
2. Set **Path** to the repository root, for example
   `/volume1/docker/easynotes`.
3. Choose the Compose file `compose.synology.yaml`.
4. Build the project, then start it.

The first build compiles Rust and can take a while on the NAS. The relay runs
database schema migrations when it starts. Check the relay container log for
`relay listening`. Container Manager should then show the relay as
**healthy**: the Compose file checks `/v1/ready` from inside the container every
30 seconds, which means the relay is up and its database answers. After
configuring the reverse proxy in the next step, check that its health endpoint
returns HTTP 200.

## 3. Add the DSM reverse proxy

In DSM, open **Control Panel → Login Portal → Advanced → Reverse Proxy** (the
menu name varies slightly by DSM version) and create a rule:

- **Source:** HTTPS, your relay DDNS hostname, port 443.
- **Destination:** HTTP, `127.0.0.1`, port `18080`.
- Assign a valid certificate to the source hostname in DSM.
- In **Custom Header**, choose **Create → WebSocket** so DSM forwards the
  `Upgrade` and `Connection` headers.

Your router should forward public TCP 443 to the NAS. Do not forward port 18080
or Postgres port 5432. Keep the relay URL as `wss://your-relay-hostname` in
clients; the app changes it to `https://` for its HTTP handshake requests.

Check `https://your-relay-hostname/v1/health` returns HTTP 200, and that
`https://your-relay-hostname/v1/ready` returns HTTP 200 with the relay's
version and the range of app protocol versions it speaks. Then point a
client at `https://your-relay-hostname` and verify it can sync; the websocket
endpoint is `/v1/sync`.

## 4. Backups and updates

In **Control Panel → Task Scheduler**, create a scheduled user-defined script
(daily is a reasonable starting point), running as a user allowed to run
Docker. Use this script as the command:

```sh
/volume1/docker/easynotes/deploy/synology/backup.sh
```

Change the path if your repository is elsewhere. It writes a compressed
Postgres dump under `/volume1/docker/easynotes-backups`, then an archive of the
attachment directory (`MEDIA_DIR`, `/volume1/docker/easynotes-data/media` by
default) with the same timestamp, checks that pg_dump finished and both
archives are intact, and only then deletes backups older than
`BACKUP_KEEP_DAYS` (30 by default). The database goes first on purpose: every
object it records as committed was on disk before the dump began, so the
archive taken after it holds its bytes. It exits non-zero when any of that fails:
tick **Send run details by email → only when the script terminates abnormally**
in the task's settings, so a failed backup is an email rather than a surprise.
Configure Hyper Backup to copy that folder to a different device or cloud
storage, with encryption and a retention of its own: a backup stored only on
the NAS will not protect against NAS failure.

A backup nobody has restored is a hope. Schedule a second task, monthly, that
restores the newest dump into a throwaway database, checks it holds the relay's
tables, and checks that the media archive with the same timestamp holds every
attachment the dump records as committed, at its recorded size — it never
touches the live database or media directory:

```sh
/volume1/docker/easynotes/deploy/synology/restore-check.sh
```

Size is read from the archive's listing, so that check costs little. Now and
then, and always after changing anything about backups, run it with
`VERIFY_DIGESTS=1` as well: it unpacks the archive into `TMPDIR` (which then
needs room for all of it) and checks every object's SHA-256 against the digest
it was committed under, the only check that proves the bytes themselves.

To restore for real, into an empty database, gunzip a dump and pipe it to
`docker exec -i easynotes-db psql -U easynotes easynotes`, and unpack the
media archive with the same timestamp into the empty media directory
(`tar -xzf easynotes-media-<stamp>.tar.gz -C /volume1/docker/easynotes-data`)
before starting the relay. Restore the two together: a database with no media
archive lists attachments whose bytes are gone, and devices that still hold
them have to upload them again. Devices that synced
after the dump was taken notice the relay went backwards, replay it and resend
what they still hold ([ADR 0004](../../docs/adr/0004-sync-engine.md), "Relay
restore recovery"); history they have already compacted away is not resent. So
treat a real restore as an incident, and check that two devices converge before
calling it over.

To update, copy/pull the new repository version, then use **Project → Action →
Build** and **Start** in Container Manager. The database directory is separate
from the containers and survives normal rebuilds. A stop is graceful: the relay
stops taking connections, closes every device's sync socket cleanly so the app
reconnects on its own, and gives uploads in flight up to eight seconds
(`RELAY_DRAIN_SECONDS`) to finish before it exits. The log says `stopped
cleanly`, or that the drain period ran out; the second costs a device a resumed
upload, not data. Do not remove the project's
data directory when cleaning up containers.

On start the relay brings its database up to date by itself, applying any new
numbered migration (`relay/migrations/`) and logging each one; there is no
step to run by hand, and a restored older backup is brought forward the same
way. Migrations only add, so rolling back to the previous release works on the
newer database. Update the relay before, or together with, the apps that use
it: an app newer than its relay may need a protocol the relay does not speak
yet, and it then stops syncing and tells its user to ask you to update
([ADR 0029](../../docs/adr/0029-compatibility-across-releases.md)).
`easynotes_relay_outdated_total` (section 5) counts the requests refused that
way.

## 5. Monitoring

The relay publishes Prometheus metrics on the NAS's loopback, at
`http://127.0.0.1:19090/metrics` (`RELAY_METRICS_PORT` in `.env` changes the
port). They are not behind DSM's reverse proxy, and should not be: they say how
many notes the relay holds and how close it is to refusing more. Check them
over SSH:

```sh
curl -s http://127.0.0.1:19090/metrics | grep -v '^#'
```

The numbers that matter most on a relay anyone with the hostname can write to
([ADR 0024](../../docs/adr/0024-relay-storage-quota.md)):

- `easynotes_relay_log_bytes` against `easynotes_relay_log_ceiling_bytes`, and
  `easynotes_relay_media_bytes` against `easynotes_relay_media_ceiling_bytes`.
  At either ceiling the relay stops taking new notes or new attachments.
  Removing attachments does not free media space yet, so that one only climbs.
- `easynotes_relay_store_up`: 0 while the database is not answering.
- `easynotes_relay_quota_refusals_total` and
  `easynotes_relay_rate_limited_total`: writes and requests the relay refused.

Anything that scrapes Prometheus's format works. With Prometheus itself (in
another container on the NAS, with `network_mode: host` so it can reach the
loopback), scrape `127.0.0.1:19090` and load
[`deploy/prometheus/alerts.yml`](../prometheus/alerts.yml), which alerts at 80%
of each ceiling, on the database or the relay going away, on refusals, and on
server errors. Route its alerts somewhere a person reads: an alert nobody sees
is a log line.

The container health check covers the relay without any of that, and Container
Manager restarts a relay that fails it; it does not tell anyone, though, and
it does not see a ceiling coming.

## 6. Check and rotate the token signing key

The key signs the short-lived access tokens devices get from the handshake
([ADR 0021](../../docs/adr/0021-stateless-relay-tokens.md)). To confirm the
relay is using yours, look for this line in the relay container's log at start:

```text
using configured relay token signing key
```

If you see `RELAY_TOKEN_SIGNING_KEY is unset` instead, the relay is using a
throwaway key it made at start. Compose refuses to start without the variable,
so that only happens when the relay is run by some other route.

**Rotating** is safe and needs nothing from anyone using the app. A device asks
for a new token every time it connects, and it only presents the token when the
socket opens. A rotation therefore costs, at most, each device's next
reconnect: a connection that was already open stays open, and one opening with
a token the new key does not recognise is refused and retried with a fresh
handshake. To rotate:

1. Generate a new key with the command in step 1, and replace the value in
   `.env`. Update your protected backup at the same time.
2. Recreate the relay container (**Project → Action → Stop**, then **Start**). If
   you run more than one relay instance, give every instance the new key before
   starting any of them. An instance still on the old key would refuse tokens
   issued by the others.
3. Check the log line above, then `/v1/health`.

Rotate after the key may have leaked (an `.env` copied somewhere it should not
be, or a backup lost) and whenever someone who had it no longer should. There
is no need to rotate on a schedule: the key protects tokens that last fifteen
minutes, and it protects no note content. Notes are end-to-end encrypted under
keys the relay never holds.

## Troubleshooting

- **Container Manager cannot build:** confirm it has access to the full
  repository, including the workspace crates and `Cargo.lock`.
- **Browser handshake fails:** check the browser page's exact origin in
  `RELAY_ALLOWED_ORIGINS` (scheme and hostname; no path), then recreate/restart
  the relay container after editing `.env`.
- **WebSocket fails but health works:** check the DSM reverse proxy's WebSocket
  custom header and confirm the client uses `wss://` through the HTTPS hostname.
- **Relay does not start:** check that Postgres is healthy and inspect the
  relay logs for a database URL, origin or trusted-proxy configuration error.
- **Log says `a trusted proxy sent a request without X-Forwarded-For`:** DSM's
  reverse proxy normally adds the header. If your DSM version does not, add a
  custom header to the reverse-proxy rule named `X-Forwarded-For` with the
  value `$proxy_add_x_forwarded_for`. Until then every device shares one rate
  limit, and more than about twenty reconnecting at once are refused for a
  minute.

Two unauthenticated status routes exist for monitoring. `/v1/health` says the
process is up and nothing else. `/v1/ready` also asks the database and answers
503 when it does not reply, with only the relay's version in the body; alert on
that one, since a relay whose database is gone still accepts connections and
then fails every write. The public write and sync routes require the
device-key handshake and use the relay's built-in rate limits.

## Licence

The relay is AGPL-3.0-or-later ([`COPYING.md`](../../COPYING.md)). Running it
unmodified for other people needs nothing more. If you change it and let other
people use your changed relay, section 13 of the licence says you must offer
them its source — publish your fork and say where in your service notice.
