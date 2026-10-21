#!/bin/sh
# Starts the smart card daemon, then the server. Extra arguments go to the server.
set -e

# Use the host's pcscd if its socket directory has been mounted in (/run/pcscd);
# otherwise run our own, which needs the USB reader passed through (/dev/bus/usb).
# Checks for a mount, not just the socket: a restarted container keeps its filesystem,
# so the socket our own pcscd left last time would otherwise look like the host's.
if mountpoint -q /run/pcscd && [ -S /run/pcscd/pcscd.comm ]; then
    echo "Using the host's pcscd."
else
    # Leftovers from the previous run's pcscd stop a new one starting cleanly.
    rm -rf /run/pcscd
    mkdir -p /run/pcscd
    echo "Starting pcscd."
    # There's no polkit in the container to ask; turn it off where pcscd supports that.
    if pcscd --help 2>&1 | grep -q -- --disable-polkit; then
        pcscd --disable-polkit
    else
        pcscd
    fi
fi

exec /usr/local/bin/tacho-card-server \
    --config "${TCS_CONFIG:-/data/config.yaml}" \
    --web-port "${TCS_WEB_PORT:-8080}" \
    "$@"
