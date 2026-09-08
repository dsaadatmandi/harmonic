#!/data/data/com.termux/files/usr/bin/bash
# Termux:Widget shortcut: bootstrap the server certificate via an otp dialog
set -u

HARMONIC_DIR="$HOME/harmonic"
CONFIG="$HARMONIC_DIR/.harmonic/config.toml"

toast() {
    command -v termux-toast >/dev/null && termux-toast -g top "harmonic: $1"
}

if [ ! -x "$HARMONIC_DIR/harmonic-client" ]; then
    toast "client binary missing, see setup docs"
    exit 1
fi

if [ ! -f "$CONFIG" ]; then
    toast "no config, create .harmonic/config.toml first"
    exit 1
fi

# start the bootstrap server on your desktop first: harmonic-server --bootstrap
OTP=$(termux-dialog -t "harmonic: enter OTP" text 2>/dev/null \
    | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')

if [ ${#OTP} -ne 64 ]; then
    toast "otp must be 64 characters"
    exit 1
fi

cd "$HARMONIC_DIR" || exit 1

# the second prompt (bootstrap port) is answered with an empty line for the default
if printf '%s\n\n' "$OTP" | ./harmonic-client --bootstrap >/dev/null 2>&1; then
    toast "bootstrap complete, run sync"
else
    toast "bootstrap failed, check the otp and that the server is running"
    exit 1
fi
