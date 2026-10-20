# Multi-Company Support and Docker Deployment

Design for serving several companies from one Tacho Card Server (TCS) and for running the TCS
headless under Docker. Reference for implementation; update it as decisions change.

Status: agreed design, not yet implemented. Last updated 2026-10-19.

## Goals

- One TCS serves several companies (clients), each with its own company cards.
- One listening port: a customer opens a single port in their firewall.
- A device only ever uses its own company's cards, and each company has its own queue.
- One company's devices can't crowd out another's.
- The TCS can run headless in Docker. The first deployment is a Synology NAS running DSM.
- A single-company install needs no company setup.
- No backward compatibility with earlier config files: nothing is deployed yet.

## Decisions

| Decision | Choice | Reason |
|---|---|---|
| How a device's company is identified | Server-side IMEI registry, behind a "company directory" interface | No firmware change; central control. Devices don't move between companies. A tracking-platform lookup can replace it later. |
| Unregistered IMEI | `ConnResponse` **Reject** | Matches the protocol's meaning for Reject ("IMEI is to be ignored; device backs off exponentially"). Firmware will be checked to make sure it backs off. |
| Company with no cards | `TryAgainLater`, never queued | No card, no point waiting. |
| Where cards live | Always in readers attached to the TCS machine | No remote card agents. |
| Connection limits | A global limit at accept, plus a per-company limit at announce | The global limit is cheap flood protection. The per-company limit stops one company crowding out others. |
| Download interval | Per company | Different customers, different needs. |
| Download location | One root folder, with a subfolder per company | `<download_path>/<company-id>/…` |
| Device authentication | None for now | Using a relayed card only yields a download from a VU the attacker is physically connected to, which needs vehicle access. `ConnAnnounce` gains room for optional fields so authentication can be added later without breaking old firmware. |
| Encryption (TLS) | Later | Devices support TLS, but certificate handling needs its own design, and it should cover position reports too. Until then downloads cross the internet in plaintext (see *Future work*). |
| Device connections | Abstracted from TCP | A future cloud relay can hand the TCS device streams over an outbound tunnel instead of a listening socket. |

## Current architecture (before this work)

- **One card pool:** `card_pool::CARD_POOL` holds every card. `smart_card::sc_monitor` adds a card when it's inserted (`add_card`) and removes it when it's pulled (`remove_by_reader`).
- **Connection handling:** `socket_server::run` accepts TCP connections; `handle_connection` runs the protocol for each one.
  - At accept: the global `connection_limit` check.
  - At `ConnAnnounce`: the download-interval check, then the device queues in `CARD_POOL`.
- **`admission`** keeps one average card session time and one backlog timer.
- **`database`** (`tcs.db`): `downloads` and `card_sessions` tables, schema version 2.
- **Config** (`config.yaml`): sections `connection`, `download`, `logging`, `appearance` and `cards`.
- **Coupling to the desktop app:**
  - **Frontend events:** core modules send these through `global_app_handle`, which uses Tauri.
  - **Async runtime:** `admission` uses `tauri::async_runtime`.
  - **Startup:** in GUI mode it's triggered by the frontend's `frontend-loaded` event.

## Design

### 1. Companies in config

```yaml
connection:
  listen_port: 7878
  connection_limit: 1000            # global, checked at TCP accept (before the device says anything)
download:
  download_path: /data/downloads    # root; each company gets a subfolder named by its id
companies:
  acme:                             # company id
    name: Acme Logistics            # display name, free to change
    connection_limit: 50            # checked at ConnAnnounce
    download_interval_days: 7       # 0 = no limit, max 60
    devices:                        # IMEI registry
      - "356938035643809"
    cards:                          # this company's cards, keyed by card number
      "<card number>":
        iccid: "..."
        # ...other card fields
unassigned_cards:                   # card number known, company not chosen yet
  "<card number>":
    iccid: "..."
```

- **Company ID:** lowercase letters, digits and `-`, validated on load. It's used for the download subfolder, the database and logs, so it must not change once data exists. The display name can change freely.
- **IMEIs:** stored as strings, to match `ConnAnnounceMessage.imei`. An IMEI may appear in only one company; a duplicate is a config error, reported at load.
- **Cards live under their company.** Where a card sits in the file is what assigns it; there's no `company` field on the card. In memory the server keeps a flat card list with each card's company filled in, so lookups by card number or ICCID don't care where a card sits.
- **New cards** (card number entered in the UI): stored under `default` while it's the only company, otherwise in `unassigned_cards` until assigned.
- **`download_interval_days`** is per company. `download.download_path` stays global as the root folder.

**First run / single company:**
- **Default company:** if config.yaml has no companies, a `default` company is created with the global connection limit and the default download interval.
- **Single-company mode:** while there's exactly one company (whatever its id), every IMEI and every card belongs to it, so a single-company install needs no setup.
- **More than one company:** a device's company comes only from the IMEI registry. An unregistered IMEI is unknown: it gets `Reject` and is recorded (`unknown_devices` table) so the Devices page can list it for assignment. New cards are unassigned until assigned.
- **Adding the second company** would suddenly reject every device of the first one that isn't registered. So creating it first registers to the existing company every IMEI in that company's download history; the UI reports how many.

### 2. Company directory

```rust
trait CompanyDirectory {
    /// Which company this device belongs to, or None if unregistered.
    fn company_for_imei(&self, imei: &str) -> Option<CompanyId>;
}
```

- **Today:** `ConfigDirectory` reads `companies.*.devices` from the config cache.
- **Later:** a tracking-platform implementation, with a local cache so a platform outage doesn't stop downloads.
- **Rule:** no other code reads the IMEI registry directly.

### 3. Card pools per company

- **`CARD_POOLS`:** `CARD_POOL` becomes a map from company ID to `Arc<CardPool>`, created from `companies` at load time. The `CardPool` internals stay the same, so FIFO queueing, priority replacement and revocation all work per company.
- **Card inserted:** `sc_monitor` looks up the card's ICCID, then its card number, then the company it sits under, and adds it to that pool.
  - **Unassigned** (or not configured yet, outside default-only mode): the card isn't pooled. It's held separately so it isn't re-registered on every reader event, logged, and shown as unassigned in the UI.
- **Card removed:** removed from whichever pool holds it. Track reader → company so removal doesn't search every pool.
- **Card moved to another company** in the UI: an idle card moves straight away; a checked-out card moves when it's released. That's how a newly identified card already gets pooled today.
- **Company removed:** its cards become unassigned. Its connected devices finish their current session; new announces are rejected.
- **Pre-filling the company:** the UI suggests a company from `company_name` (read from the card's EF_Identification), but a person confirms it. Names vary too much to match automatically.

### 4. Connection flow

```
TCP accept
  └─ global connection_limit exceeded? → TryAgainLater (global estimate), close
ConnAnnounce (10 s timeout)
  ├─ directory: IMEI → company?          none → Reject, close
  ├─ company download interval not met?   → TryAgainLater (until allowed), close
  ├─ company has no cards?                → TryAgainLater (fixed delay), close
  ├─ company connection_limit reached?    → TryAgainLater (company estimate), close
  └─ queue in the company's pool          → Accepted

The connection limit is checked last because checking it and claiming the slot happen
under one lock, so two devices can't both take the last slot.
```

- **Per-company connection count:** incremented when a device is admitted to a company, decremented when the connection ends (including Reject and TryAgainLater paths that never admit).
- **What it counts:** all of that company's connections, from announce to disconnect, including devices that have released their card and are uploading a download. That's what occupies connection slots.
- **Last card removed:** devices still waiting for their *first* card are sent `TryAgainLater` and disconnected, since there's nothing left to wait for. A device whose card was pulled mid-session already gets `Abort(CardRemoved)`; it's treated the same way if no card remains in its company.
  - **Firmware check:** this sends `ConnResponse` after `Accepted`, which the server already does unprompted for over-limit connections. Confirm the firmware accepts it at that point.

### 5. Admission per company

- **Per-company state:** `admission` keeps the average session time, queue estimate and backlog timer for each company, using that company's pool (`card_count`, `idle_count`, `waiting_count`).
- **Over the global limit,** before announce: the company isn't known yet, so the retry estimate uses totals across all pools. It's a flood guard, so a rough figure is fine.
- **Over a company's limit:** the retry estimate comes from that company's own queue and backlog.
- **Persistence:** `card_sessions` gains a `company` column, so each company's average survives restarts.

### 6. Downloads and the database

- **Saved to:** `<download_path>/<company-id>/<VRN>_<IMEI>_<timestamp>[_<tag>].ddd`. The filename stays as it is today.
- **Interval check:** uses the device's company's `download_interval_days`, and only considers that company's downloads.
- **Schema v3:**
  - `downloads.company TEXT` and `card_sessions.company TEXT` columns;
  - existing rows are backfilled with `default`;
  - an index on `downloads (company, imei, local_date)`.
- **`retain_failed_downloads`** stays global.

### 7. Device transport

`handle_connection` takes any `AsyncRead + AsyncWrite` stream, plus a device label for logging, instead of a `TcpStream` and `SocketAddr`. Everything after the stream (announce, queueing, relay, downloads) doesn't care where it came from. The TCP listener is one source of streams; a cloud relay tunnel can be another later.

### 8. Protocol

- **`ConnAnnounce`:** after the firmware version, allow optional trailing fields as `type (1 byte), length (1 byte), value`.
  - The server ignores types it doesn't recognise.
  - Old firmware sends none and is unaffected.
  - Nothing uses this yet; it's room for authentication or other metadata later.
- **`ConnResponse` Reject (0x00):** sent for unregistered IMEIs. Check that the device firmware handles it with the exponential backoff described in `device server protocol.md`.
- **`device server protocol.md` is out of date.** It still shows a 2-byte retry time (now 4 bytes, `u32`) and placeholder bodies for several messages. Update it alongside this work.

### 9. Logging

Every connection log line from announce onwards gains `company=<id>`, plus `[CARD_POOL]` lines for pool changes.

### 10. Desktop UI

Header navigation between three pages: **Readers** (the existing view; kept alive so its live state survives switching pages), **Companies** and **Devices**.

- **Companies page:** list, add, edit and delete companies (name, id, connection limit, download interval). The id is suggested from the name and can't be changed after creation. Deleting asks for confirmation; its cards become unassigned and its devices unknown; the last company can't be deleted. Each company lists its cards with a "Move to" picker (another company, or unassigned); unassigned cards are listed separately.
- **Devices page:** unknown devices (rejected, with first/last seen and attempt count) with an "Assign to" picker; registered devices with a company picker and an unregister button; add a device by IMEI (15 digits, Luhn check).
- **Status per company** (cards idle / busy, devices waiting, estimated wait, average session): not built yet.

## Docker deployment

### Slim server binary

The Tauri app links GTK and WebKit, which a server container shouldn't need. The crate has a `desktop` Cargo feature, on by default:

- **`cargo build`** (or `npm run tauri build`): the desktop app, always with a window. No web UI.
- **`cargo build --no-default-features`:** the headless server, always with the web UI, with no Tauri, WebKit or GTK crates. On Windows it keeps its console. This is the Docker binary.

There's no `--headless` flag: install the desktop app for a desktop, the headless build for a server.

How it's untangled:
- **Events:** `global_app_handle::emit` sends to the Tauri window in the desktop build and to web clients in the headless build.
- **Async runtime:** `rt` owns the tokio runtime; the desktop app hands it to Tauri (`tauri::async_runtime::set`).
- **Tauri commands:** `#[cfg_attr(feature = "desktop", tauri::command)]`, so they're plain functions in the server build; the web server calls them directly.
- **Desktop-only code:** window, tray, single instance, "open logs folder" are in `desktop.rs` / behind the feature.
- **Unit tests** run with `cargo test --no-default-features --lib`; the desktop build's test binary doesn't start on Windows (a Tauri manifest issue).

### Container

`Dockerfile`, `docker/entrypoint.sh` and `docker-compose.yml` at the repo root.

- **Build:** three stages: the Vue UI (`quasar build`), the headless binary (`cargo build --release --no-default-features`, embedding the UI), and the runtime image.
- **Base image:** `debian:bookworm-slim` with `pcscd`, `libccid`, `libssl3`, `ca-certificates` and `tzdata`. The entrypoint starts `pcscd` (with `--disable-polkit` where supported), unless the host's pcscd socket is mounted at `/run/pcscd`, then runs the server with `--config /data/config.yaml --web-port $TCS_WEB_PORT`. Extra container arguments are passed to the server.
- **Volume:** `/data` holds `config.yaml` (with the generated admin password), `tcs.db`, `logs/` and `downloads/`. Logs also go to stdout.
- **Ports:** 7878 for devices (must match `connection.listen_port`), 8080 for the web UI (`TCS_WEB_PORT`).
- **USB access:** `devices: /dev/bus/usb` and `device_cgroup_rules: c 189:* rmw`. The cgroup rule covers any USB device, so unplugging and replugging the reader (which changes its device path) keeps working.
- **Time zone:** set `TZ`. The download interval uses *local* calendar dates, and containers default to UTC.
- **Shutdown:** the server handles `SIGTERM` as well as Ctrl+C; Compose also sets `init: true`.
- **Health check:** the web port accepting connections.
- **Architecture:** CI builds `linux/amd64`. If the NAS is `aarch64`, add `linux/arm64` to the workflow's platforms (slow under emulation) or use an arm64 runner.

### CI (`.github/workflows/build.yml`)

- **Desktop:** Windows MSI and Ubuntu AppImage, as before.
- **Headless:** `tcs-headless-windows-amd64.exe` and `tcs-headless-linux-amd64` (built on Ubuntu 22.04; needs `libpcsclite1`, `libssl3` and `libudev1`). The Linux job also runs the unit tests.
- **Docker:** builds the image on every run; on `v*` tags pushes `ghcr.io/<owner>/tacho-card-server` tagged with the version, `major.minor` and `latest` (not for pre-releases).
- **Release:** on `v*` tags, the MSI, AppImage and both headless binaries are attached.

### Synology DSM

- **Deployment:** deploy as a Container Manager **Project** (Compose). The Container Manager UI can't pass USB devices into a container; Compose can.
- **Tested so far:**
  - A `debian:bookworm-slim` container with the USB options above.
  - Inside it, `pcsc_scan` detected the **HID Global OMNIKEY 3x21** reader through PC/SC plug-and-play.
- **Assumed:** inserting a card shows its ATR. Not tested, but the reader is detected through the same PC/SC path.
- **Still to confirm:** the NAS architecture (`uname -m`), before building the image.

See `docker-compose.yml` for the Compose file.

## Users and the web UI

### Users

- **Storage:** a `users` table in `tcs.db`: username, Argon2id password hash (PHC string), created/updated times. Plaintext passwords are never stored there.
- **Built-in `admin`:** always exists. Its password is `admin_password` in config.yaml, in plaintext, and the database is made to match it on every startup.
  - **Changing it:** edit config.yaml and restart.
  - **Missing or blank:** a random 20-character password is generated, written to config.yaml and applied to the database. Blanking it is how a lost admin password is recovered.
  - config.yaml now holds a secret: keep it (and the Docker `/data` volume) readable only by the service.
- **Logins fail closed:** if the database can't be read, a login is refused, unlike the rest of the database, which fails open so it never blocks a download.

### Desktop vs headless

| Mode | UI | Who you are |
|---|---|---|
| Desktop app (Tauri window) | The Vue UI in the window | Always `admin`; no login |
| Headless build (`--no-default-features`, Docker) | The same Vue UI served as a web app | Any user, after a login screen |

### Web server (headless only)

- **Separate port** from the device port: 8080, changed with `--web-port <port>`, on all interfaces. Customers open only the device port to the internet; the admin UI stays on the LAN.
- **Framework:** `axum`, on the server's existing `tokio` runtime.
- **Static UI:** the built Vue app (`dist/spa`) is embedded in release builds with `rust-embed` (read from disk in debug builds), so the Docker image needs no extra files. Build the frontend before the binary. For frontend work, `quasar dev` proxies `/api` to a headless backend on port 8080.
- **API:** `POST /api/invoke/<command>` with the same JSON arguments as the desktop app's Tauri `invoke`, backed by the same Rust functions. One implementation serves both.
- **Events:** `GET /api/events` is a Server-Sent Events stream carrying the same events the desktop app gets from Tauri (`global-cards-sync`, `global-card-config-updated`, `global-config-*`, `server-status`, …). `global_app_handle::emit` sends each event to the Tauri window (if any) and to a broadcast channel the streams read. A page that (re)connects calls `resend_state` to get current state.
- **Frontend:** `src/backend.ts` has the same `invoke`, `listen` and `emit` as `@tauri-apps/api`, using Tauri in the desktop app and `fetch`/SSE in a browser. Components import from it, never from Tauri directly (except the desktop-only folder picker).
- **Desktop-only features hidden in the browser:** minimise to tray, "Open logs folder", and the folder picker (the download path is typed instead). The theme is the server's saved one.

### Login and sessions

- `POST /api/login` (username, password) sets a session cookie: random token, `HttpOnly`, `SameSite=Strict`. `POST /api/logout` ends it, and `GET /api/session` says who's logged in.
- **Sessions are kept in memory:** a restart logs everyone out. They expire after 12 hours of inactivity.
- **Brute-force protection:** after 5 failed logins from one address, each further attempt must wait, from 1 s doubling up to a minute (`429` with `Retry-After`). Behind a reverse proxy every login comes from the proxy's address, so the limit is then effectively global. A login for an unknown username takes as long as a wrong password, so timing doesn't reveal usernames.
- **Changing a user's password** ends their other sessions; **deleting a user** ends all of theirs.
- **CSRF:** the cookie is `SameSite=Strict` and every POST must be JSON, which other sites can't send without CORS.
- **Every API call except login needs a valid session.**

### Users page

Lists users. You can add a user, change a user's password, and delete a user. `admin` can't be deleted, and its password is changed only in config.yaml; the page says so.

**No roles:** only administrators get accounts, because configuration changes shouldn't be taken lightly, so every user can do everything. Usernames are 1–32 of letters, digits, `.`, `_`, `-` (any capitalisation of `admin` is reserved); passwords are at least 8 characters.

### Plain HTTP

Without TLS, passwords cross the LAN in cleartext. Until TLS is built in, keep the web port LAN-only, or put it behind a TLS reverse proxy. On Synology, DSM's built-in reverse proxy with a Let's Encrypt certificate does this with no extra software.

## Phases

1. **Multi-company core**
   - [x] `companies` config section with cards nested under each company, validation, and a `default` company on first run
   - [x] `CompanyDirectory` with the config implementation
   - [x] Card pool per company; insert/remove routing; unassigned cards held out of every pool
   - [x] Moving a card to another company while it's in a reader: idle or unassigned cards move straight away; a card in use moves when its session ends
   - [x] Per-company connection limit and download interval at announce; Reject for unregistered IMEIs; no cards → TryAgainLater
   - [x] Per-company admission state
   - [x] Download subfolders; schema v3 with `company` columns
   - [ ] Generic stream in `handle_connection`
   - [ ] Optional trailing fields in `ConnAnnounce`
   - [ ] `company=` in logs
   - [x] Desktop UI: Companies page (companies and card assignment), Devices page (unknown and registered devices)
   - [ ] Desktop UI: per-company status
   - [ ] Update `device server protocol.md`
2. **Docker**
   - [x] `desktop` feature; Tauri-free server build with `--no-default-features`
   - [x] Events decoupled from Tauri; own tokio runtime (`rt`)
   - [x] `SIGTERM` handling
   - [x] Dockerfile, entrypoint, health check, Compose example
   - [x] CI: headless binaries, Docker image (pushed to GHCR on `v*` tags)
   - [ ] Test on the Synology NAS
3. **Users and web UI**
   - [x] `users` table; built-in `admin` synced from `admin_password` in config.yaml, generated when missing or blank
   - [x] Event emission decoupled from Tauri (`global_app_handle::emit` → window and web clients)
   - [x] `backend.ts` wrapper in the frontend; hide desktop-only features in a browser
   - [x] `axum` web server in headless mode: embedded UI, `/api/invoke/<command>`, `/api/events` (SSE)
   - [x] Login, sessions, logout, brute-force delay; login screen
   - [x] Users page (add, change password, delete)
   - [x] `--web-port` command-line argument
4. **Later:** tracking-platform company directory; cloud relay; TLS.

## Open questions

- **Synology:** which architecture is the NAS (`uname -m`)? Needed for phase 2.
- **Firmware:** confirm `ConnResponse` Reject is handled with exponential backoff (being checked), and that a `TryAgainLater` sent after `Accepted` is accepted (see §4).
- **Lowering a company's limit:** if it drops below the number already connected, existing connections are left alone and only new announces are refused. Confirm that's acceptable.

## Future work

- **TLS:** downloads currently cross the internet in plaintext between device and TCS, and driver activity records are personal data under GDPR. Options:
  - TLS on the existing port (needs a certificate design; a private CA trusted by the device firmware is likely simplest);
  - the cloud relay, with TLS on both legs;
  - a private APN or VPN.
- **Tracking platform:** look up which company an IMEI belongs to, and later manage card assignments centrally.
- **Cloud relay:** the TCS connects outbound and receives device streams through it. With the generic stream (§7), no port is open at the customer.
