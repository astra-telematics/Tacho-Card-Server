# Tacho Card Server, headless: the server build (no Tauri, no webview) with its web UI.
#
#   docker build -t tacho-card-server .
#
# See docker-compose.yml for running it, and "Docker deployment" in
# `multi-company design.md` for the design.

# ---- Frontend: the Vue UI, embedded in the binary. Platform-independent output, so it
# builds natively even when the image is cross-built for another architecture.
FROM --platform=$BUILDPLATFORM node:20-bookworm-slim AS frontend
WORKDIR /src
COPY package.json package-lock.json ./
# Install scripts run `quasar prepare`, which needs the sources; run it at build instead.
RUN npm ci --ignore-scripts
COPY . .
RUN npx quasar build

# ---- Server binary.
FROM rust:1-bookworm AS server
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev libpcsclite-dev libudev-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY src-tauri src-tauri
# rust-embed reads the UI from ../dist/spa relative to src-tauri.
COPY --from=frontend /src/dist/spa dist/spa
WORKDIR /src/src-tauri
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/src-tauri/target \
    cargo build --release --locked --no-default-features \
    && cp target/release/tacho-card-server /usr/local/bin/tacho-card-server

# ---- Runtime.
FROM debian:bookworm-slim
# pcscd + libccid: the smart card daemon and USB CCID reader driver (OMNIKEY etc.).
# tzdata: download intervals count calendar days in local time; set TZ.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pcscd libccid libpcsclite1 libssl3 libudev1 ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*
COPY --from=server /usr/local/bin/tacho-card-server /usr/local/bin/tacho-card-server
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# /data holds config.yaml (including the generated admin password), tcs.db and logs/.
# /downloads holds the downloaded tachograph files, one subfolder per company; it's
# separate so it can be mounted from wherever they should go (e.g. a shared folder).
VOLUME /data /downloads
# The ports and download folder are fixed here, overriding config.yaml, and shown
# read-only in the UI: then the exposed ports always match what the server listens on,
# and downloads always land in the volume. To use other ports, change the host side of
# the port mappings (docker-compose.yml), not these.
ENV TZ=UTC \
    TCS_LISTEN_PORT=7878 \
    TCS_WEB_PORT=8080 \
    TCS_DOWNLOAD_PATH=/downloads
# Devices, and the web UI.
EXPOSE ${TCS_LISTEN_PORT} ${TCS_WEB_PORT}

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s \
    CMD bash -c "</dev/tcp/127.0.0.1/${TCS_WEB_PORT}" || exit 1

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
