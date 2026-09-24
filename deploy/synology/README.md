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
- Set `NAS_DATA_DIR` to a persistent shared-folder path, for example
  `/volume1/docker/easynotes-data`.

Create the data directory in File Station and ensure the Container Manager
service can write to it. Postgres stores its database under
`$NAS_DATA_DIR/postgres`.

## 2. Build and start the project

In **Container Manager → Project → Create**:

1. Name it `easynotes-relay`.
2. Set **Path** to the repository root, for example
   `/volume1/docker/easynotes`.
3. Choose the Compose file `compose.synology.yaml`.
4. Build the project, then start it.

The first build compiles Rust and can take a while on the NAS. The relay runs
database schema migrations when it starts. Check the relay container log for
`relay listening`. After configuring the reverse proxy in the next step, check
that its health endpoint returns HTTP 200.

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

Check `https://your-relay-hostname/v1/health` returns HTTP 200. Then point a
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
Postgres dump under `/volume1/docker/easynotes-backups`. Configure Hyper Backup
to copy that folder to a different device or cloud storage: a backup stored
only on the NAS will not protect against NAS failure. Test restoring a dump
before relying on it. To restore into an empty database, gunzip a dump and pipe
it to `docker exec -i easynotes-db psql -U easynotes easynotes`.

To update, copy/pull the new repository version, then use **Project → Action →
Build** and **Start** in Container Manager. The database directory is separate
from the containers and survives normal rebuilds. Do not remove the project's
data directory when cleaning up containers.

## 5. Check and rotate the token signing key

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
  relay logs for a database URL or origin configuration error.

The relay currently has no authentication on `/v1/health`; it returns only a
plain success status. Its public write and sync routes require the device-key
handshake and use the relay's built-in rate limits.
