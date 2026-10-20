# Tacho Card Server

![Tacho Card Server](src/assets/logo.svg 'Tacho Card Server')

The application is based on [Tacho-Bridge-App](https://github.com/flespi-software/Tacho-Bridge-App/)
It is designed for use with Astra Telematics vehicle tracking devices

## Specifications

Project uses [Tauri framework](https://tauri.app/) = [Rust](https://www.rust-lang.org/) + [Typescript](https://www.typescriptlang.org/) + [Vue 3](https://vuejs.org/) + [Quasar](https://quasar.dev/)

Quasar will be used as an interface, buttons, menu, etc. It was decided to abandon the native solution offered by Tauri due to possible difficulties with adaptation on different OS, and this will also facilitate the implementation of a mobile interface if required. Also, the native interface requires a bunch of imports that are already in Quasar.

## Getting started

Firstly it is needed to install [Rust](https://v2.tauri.app/start/prerequisites/).

Init project from the root directory

```
npm install
```

Cargo can be updated only from the ./src-tauri directory

```
cargo update
```

Then it is needed fetch Cargo dependeces from the rust directory

```
cd src-tauri
cargo fetch
```

run project

```
npm run tauri dev
```

Build

```
# default build command for the current OS.
npm run tauri build

# Build MacOS without signature and notarization.
npm run tauri build -- --target aarch64-apple-darwin    # targets Apple silicon machines.
npm run tauri build -- --target x86_64-apple-darwin     # targets Intel-based machines.
npm run tauri build -- --target universal-apple-darwin  # unversal app for x86 and ARM machines.
```

### MacOS code signing and notarization

Сreate a .env file with the variables described below with the specified credentials. IMPORTANT: this file is added to .gitignore, it will not be sent to the repository for the security purposes.

```
APPLE_IDENTITY="Developer ID Application: Your Name (YOUR_TEAM_ID)"
APPLE_TEAM_ID=YOUR_TEAM_ID
APPLE_ID=your.email@example.com
APPLE_PASSWORD=your-app-specific-password

# Enable notarization in Tauri 2.0
ENABLE_NOTARIZE=true
```

Then just run the _build-mac.sh_ script which will check for the necessary variables, settings and start building a universal bundle that can run on all Mac architectures (x86 & ARM). The binary file will contain code for both architectures, the required one will be selected for launch.

If everything went well, you will see something like:

```
🔄 Restoring original configuration
✅ Build completed successfully!
📊 Application architecture information:
Architectures in the fat file: ./src-tauri/target/universal-apple-darwin/release/bundle/macos/tcs.app/Contents/MacOS/tacho-card-server are: x86_64 arm64

🏁 Script execution completed
```

### Linux building & using

Minimum supported versions: **Ubuntu 22.04** / **Debian 12 (bookworm)** or newer.
Tauri v2 requires `libwebkit2gtk-4.1-dev` which is not available on older distributions.

To install system libraries you need _sudo_ administrator rights. Please be careful when installing new packages and dependencies.

**[Tauri v2 Core Dependencies](https://v2.tauri.app/start/prerequisites/)**

```
sudo apt install -y build-essential curl wget pkg-config file \
  libssl-dev libxdo-dev libudev-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

**PCSC Smart Card Support**

```
sudo apt install -y pcscd libpcsclite-dev libccid usbutils
```

**AppImage build dependencies**

```
sudo apt install -y squashfs-tools fuse
```

### Headless build (servers and Docker)

There are two builds: the **desktop app** above (a window and tray icon), and the
**headless server**, which has no window and serves the same UI as a web app with a
login. Install whichever fits; the desktop app has no headless mode.

The headless build doesn't use Tauri, so it needs no GTK or WebKit, only PC/SC and
OpenSSL (`sudo apt install -y build-essential pkg-config libssl-dev libpcsclite-dev libudev-dev pcscd libccid`):

```
npm run build                                   # the web UI, embedded in the binary
cd src-tauri
cargo build --release --no-default-features     # target/release/tacho-card-server
```

Run it with `--config <path>` to choose the config file (data, logs and downloads go
next to it) and `--web-port <port>` for the web UI (default 8080). Log in as `admin`
with the `admin_password` from config.yaml, which is generated on first start. The web
UI is plain HTTP: keep it on the LAN or behind a TLS reverse proxy.

**Docker:** `docker compose up -d` with [docker-compose.yml](docker-compose.yml) (pulls
`ghcr.io/astra-telematics/tacho-card-server`, or uncomment `build: .`). Data is in
`./data`, and the admin password is in `./data/config.yaml` after the first start.

Unit tests run in the headless build: `cargo test --no-default-features --lib`.

## Icon generating & customizing

The project stores icons in a directory: **src-tauri/icons**

[Detailed description of icon generation and their characteristics from Tauri](https://tauri.app/v1/guides/features/icons/)

Tauri has a very convenient and super-simple tool for generating all the necessary icons for an application. What you need to do:

1. Upload a PNG image with a transparent background to the image directory "**src-tauri/icons**". The resolution should be **1024x1024**, this is the maximum icon size for MacOS, so that everything is displayed beautifully.
2. Run the Tool for generating icons from the <u>root of the project</u>:

```
npm run tauri icon src-tauri/icons/app-icon.png
```

**That's it. All the necessary icons of all sizes for all platforms will be generated.**

## Usage

TCS is a transparent APDU proxy between a remote Vehicle Unit (tachograph) and a physical tachograph card in a local smart card reader. The authentication ceremony is driven by the VU; TCS only relays bytes and extracts data that passes through.

### Linking a card

Before a card can be used, it must be linked in TCS:

1. Insert the card into a reader. TCS detects it and shows its **ICCID**.
2. Click the **Add Card** button (or use link mode on the detected card) to bind the physical card to a **card number** in the config.
3. Card numbers are 16 alphanumeric uppercase characters per Annex 1C regulation (EU 2016/799).

Without linking, an inserted card appears as "UNKNOWN CARD" and cannot authenticate.

### Card information display

Once linked and authenticated at least once, the reader block shows, under the reader name:

- **Name** — user-defined label (e.g. fleet identifier)
- **Card number** followed by `(generation | card type)` in grey, e.g. `4200000000525000 (Gen2 v2)`
- **Company name** and **company address** (icons 🏢 and 📍)
- **Expire** date, red if already past
- **Last auth** — last completed authentication time and its status (`success` green, `fail` red, `processing...` yellow during an active APDU exchange)

### Card data is only available during / after authentication

Tachograph cards protect their content with access conditions. Files like `EF_Identification` (card number, company name/address, expiry date) and `EF_Application_Identification` (card type, structure version) are readable **only** over Secure Messaging established by the Vehicle Unit during mutual authentication.

TCS does **not** hold any cryptographic keys (those are issued by EUR CA to certified VU/workshop manufacturers only), so it cannot originate SM commands on its own. Instead it passively observes the APDU stream passing through:

- VU issues `SELECT EF` + `READ BINARY` wrapped in SM after auth
- The card's response contains `DO'81` (plain value) + `DO'8E` (MAC) — tachograph SM protects integrity but **does not encrypt** the file content
- TCS parses the plaintext from `DO'81` and persists recognised fields

Currently recognised files:

| FID    | File                            | Extracted fields                                                                                                                             |
| ------ | ------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `0520` | `EF_Identification`             | cardExpiryDate → `expire`, companyName → `company_name`, companyAddress → `company_address`                                                  |
| `0501` | `EF_Application_Identification` | typeOfTachographCardId → `card_type`, cardStructureVersion → `structure_version` (keep highest on Gen2 cards that expose both Gen1 and Gen2) |

If the card has never been authenticated through TCS, these fields stay `null` in the config and the UI shows only the fields that are available pre-auth (ICCID, card number, name).

### Time storage and display

`last_auth` and `expire` are stored as UTC unix seconds. The UI renders them in the **local timezone of the machine**, so the same config file shown on computers in different regions displays the corresponding local time. Format: `YYYY-MM-DD HH:MM:SS`.

### Config and log location

Both files live in the user's Documents directory:

- **Config:** `~/Documents/tcs/config.yaml` — server host, app identifier, per-card data (iccid, card number, expire, company info, last_auth, …)
- **Log:** `~/Documents/tcs/log.txt` — rolling file log written by `fern`; contains APDU traces (at DEBUG), connection events (`[CONN] phase=… status=…`), and sniffer field extractions

On first run both are created automatically; if the OS denies write access, the UI surfaces a notification.

## License

[MIT](LICENSE) license.
