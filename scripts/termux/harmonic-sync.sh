#!/data/data/com.termux/files/usr/bin/bash
# Termux:Widget shortcut: run one manual harmonic sync on launch
set -u

HARMONIC_DIR="$HOME/harmonic"
CONFIG="$HARMONIC_DIR/.harmonic/config.toml"
CERT="$HARMONIC_DIR/.harmonic/server.crt"

toast() {
    command -v termux-toast >/dev/null && termux-toast -g top "harmonic: $1"
}

if [ ! -x "$HARMONIC_DIR/harmonic-client" ]; then
    toast "client binary missing, see setup docs"
    exit 1
fi

if [ ! -f "$CONFIG" ]; then
    toast "no config, run setup first"
    exit 1
fi

if [ ! -f "$CERT" ]; then
    toast "no server certificate, run bootstrap first"
    exit 1
fi

cd "$HARMONIC_DIR" || exit 1

if OUTPUT=$(./harmonic-client 2>&1); then
    toast "sync complete"
else
    toast "sync failed"
    command -v termux-notification >/dev/null && \
        termux-notification --title "harmonic sync failed" --content "$OUTPUT"
    exit 1
fi

# surface newly synced files in reader apps
if command -v termux-media-scan >/dev/null; then
    SYNC_PATH=$(sed -n 's/^sync_path = "\(.*\)"/\1/p' "$CONFIG")
    [ -d "$SYNC_PATH" ] && termux-media-scan -r "$SYNC_PATH"
fi
