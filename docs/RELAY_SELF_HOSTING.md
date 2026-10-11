# Self-hosting the OpenPal sync relay

This documents packaging for **your own** relay instance. OpenPal does not
operate a default hosted one — see "What this is not" at the end for why,
and `docs/design/MULTI_DEVICE_SYNC.md`'s "Ops / non-engineering considerations"
for the reasoning behind that choice. Everything below is for someone who
wants to run their own relay, for themselves or a group they choose to
support.

## Locked decisions

- **No TLS termination in the relay process.** It's a plain HTTP server;
  putting TLS in front of it is your job (see "TLS is mandatory" below).
- **No authentication on any route.** Anyone who can reach the relay can
  push, read, and delete any mailbox by guessing/enumerating its id. The
  network path to the relay — your reverse proxy, your firewall rules — is
  the entire access-control layer. There is nothing to configure inside the
  relay itself for this.
- **The relay is cryptographically blind.** It stores and forwards opaque
  ciphertext and metadata (mailbox IDs, envelope sizes, timestamps) — it
  never sees plaintext conversations, titles, or any key. This is true
  regardless of how you deploy it.

## Deploy path 1: Docker Compose

```
git clone https://github.com/myopenpal-ai/openpal.git
cd openpal
docker compose up --build -d
```

This builds `crates/relay/Dockerfile` and starts one `relay` service,
publishing port `8787`, with a named volume (`relay-data`) persisting the
SQLite mailbox file across restarts. Check it came up healthy:

```
docker compose ps          # look for "healthy" in the STATUS column
curl -f http://127.0.0.1:8787/health
```

Change the published port, `RUST_LOG` level, or rate limit by editing the
`environment:` block in `docker-compose.yml` before starting.

## Deploy path 2: bare metal + systemd

```
cargo build --release -p openpal-relay
sudo install -m 755 target/release/openpal-relay /usr/local/bin/openpal-relay

sudo useradd --system --no-create-home openpal-relay
sudo mkdir -p /var/lib/openpal-relay
sudo chown openpal-relay:openpal-relay /var/lib/openpal-relay

sudo mkdir -p /etc/openpal-relay
sudo tee /etc/openpal-relay/relay.env <<'EOF'
RELAY_DB_PATH=/var/lib/openpal-relay/relay.sqlite
PORT=8787
RUST_LOG=info
RELAY_RATE_LIMIT_PER_MINUTE=600
EOF

sudo cp crates/relay/openpal-relay.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now openpal-relay
journalctl -u openpal-relay -f
```

The unit runs the relay as its own unprivileged system user, with
`ProtectSystem=strict` limiting write access to `/var/lib/openpal-relay`
only — see the unit file's own comments for what hardening it deliberately
does and doesn't include.

## TLS is mandatory — reverse proxy required, and why

This isn't just a recommendation: **the OpenPal client itself refuses to
connect** to a non-loopback relay that isn't `https://`.
`crates/core/src/sync/mailbox_client.rs`'s `validate_relay_url` accepts
plain `http://` only for `127.0.0.1`/`::1`/`localhost` — anything else over
plain HTTP is rejected at save time in Settings → Advanced → Sync, before
the client ever tries to talk to it. So a relay reachable over any network
beyond your own machine needs TLS terminated somewhere in front of it, or
no client will ever pair with it.

The smallest example, using [Caddy](https://caddyserver.com) for its
automatic Let's Encrypt provisioning (keeps this shorter than an
nginx+certbot setup):

```
# /etc/caddy/Caddyfile
relay.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

That's the entire config — Caddy handles certificate issuance and renewal
on its own. Point it at wherever your relay is actually listening (the
Docker Compose path's published port, or the systemd path's `PORT`).

## Point a client at it

Nothing new to build here — this is the existing Settings → Advanced →
Sync UI. Open it, enter your relay's public URL
(`https://relay.example.com`) into the "Server" field, and save. If the
URL fails `validate_relay_url` (wrong scheme for a non-loopback host,
malformed URL), the save itself fails with an error — it won't silently
fall back to the default relay.

## Backup and data

- **Docker Compose**: the SQLite file lives inside the `relay-data` named
  volume, at `/home/relay/data/relay.sqlite` inside the container.
- **Bare metal**: `/var/lib/openpal-relay/relay.sqlite` (or wherever
  `RELAY_DB_PATH` points).
- **What's actually in it**: one `mailbox` table
  (`crates/relay/src/store.rs`) — ciphertext blobs, opaque mailbox IDs, and
  timestamps. Never plaintext, never a key, by the same design as
  everywhere else in this feature.

Back it up if you want to, but frame that decision correctly: this file
only ever holds envelopes that haven't been delivered and acked yet — a
short-lived queue, not a system of record. Each device's own local
`app.sqlite` is the actual copy of your data. Losing the relay's database
loses, at most, a window of in-flight envelopes that hadn't synced yet.

## Upgrading

The relay's SQLite schema has no version/migration ladder
(`crates/relay/src/store.rs` uses plain `CREATE TABLE IF NOT EXISTS`) —
upgrading is just: stop the service, replace the binary or image, start it
again. No separate migration step exists or is needed.

One real constraint: don't run two different relay binary versions
concurrently against the same SQLite file. `MailboxStore` is a single
in-process `Mutex<Connection>`, not a multi-writer design — this is a
non-issue for the normal Docker Compose (`up --build -d` recreates the one
container) or systemd (`systemctl restart`) upgrade flow, since both
naturally stop the old process before starting the new one; it only
matters if you're doing something unusual like a manual blue-green swap.

## Configuration reference

| Env var | Default | Meaning |
|---|---|---|
| `RELAY_DB_PATH` | `./relay.sqlite` | SQLite mailbox file location. |
| `PORT` | `8787` | TCP port the relay listens on (all interfaces — `0.0.0.0`). |
| `RUST_LOG` | (unset → default) | Log level, e.g. `info`, `debug`. |
| `RELAY_RATE_LIMIT_PER_MINUTE` | `600` | Per-source request budget — see `crates/relay/src/rate_limit.rs`'s doc comment for the reasoning and its `X-Forwarded-For`-vs-`ConnectInfo` trust assumption (only correct if the relay is reachable *only* through your configured reverse proxy — never expose it directly alongside one). |

## What this is not

- **Not OpenPal's own hosted service.** There is no default instance; every
  relay is someone's own deployment.
- **No abuse-handling SLA, no support, no uptime guarantee.** Self-hosted
  means self-supported. If you run this for people beyond your own
  devices, deciding and publishing your own retention/privacy/abuse
  policies for them is your responsibility, not something this document or
  the relay's code does for you.
- **No authentication beyond the network path.** Anyone who can reach your
  relay's HTTP port can read, write, and delete any mailbox by id. Your
  reverse proxy and firewall are the entire access-control layer — there is
  no account system, no per-device credential, nothing to configure inside
  the relay.
- **No content moderation, even in principle.** The relay is
  cryptographically blind — it cannot inspect, and therefore cannot
  moderate, anything it stores.
- **Rate limiting is best-effort and per-source only** — a floor against
  one hammering client, not a defense against a distributed or heavily-NATed
  abuser. See the configuration reference above for its trust assumption.
