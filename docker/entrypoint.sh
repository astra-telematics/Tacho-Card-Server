#!/bin/sh
# Starts the smart card daemon, then the server. Extra arguments go to the server.
set -e

# Use the host's pcscd if its socket has been mounted in (/run/pcscd); otherwise run
# our own, which needs the USB reader passed through (/dev/bus/usb).
if [ -S /run/pcscd/pcscd.comm ]; then
    echo "Using the host's pcscd."
else
    mkdir -p /run/pcscd
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
