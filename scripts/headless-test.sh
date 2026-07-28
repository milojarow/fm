#!/usr/bin/env bash
# Runs fm inside a nested headless sway compositor and screenshots it, so GUI
# behaviour can be exercised without touching the operator's live session.
#
# Usage:  scripts/headless-test.sh <shot-name> [typed-keys]
#
# Env:
#   RES=1600x900          nested output resolution
#   START_DIR=<path>      directory fm opens (default: this checkout)
#   KEYS_ARGS="-k space"  raw wtype arguments, for named keys and chords
#   SEED_CLIPBOARD=<text> primes the nested clipboard with wl-copy before fm
#                         starts, so the paste path is exercised the way it
#                         would be from another application
#   AFTER_CMD=<command>   runs with WAYLAND_DISPLAY set, before teardown —
#                         the only chance to inspect the clipboard, which dies
#                         with the compositor
#
# Screenshots and logs land in target/headless/.
set -u

REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$REPO/target/headless"
mkdir -p "$OUT"

SHOT="${1:?usage: headless-test.sh <shot-name> [typed-keys]}"
KEYS="${2:-}"
RES="${RES:-1600x900}"
START_DIR="${START_DIR:-$REPO}"

printf 'output HEADLESS-1 resolution %s\n' "$RES" > "$OUT/sway.conf"
ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | sort > "$OUT/sockets-before"

# renderD128 is the Intel node and is not optional: SwayFX's renderer is
# EGL-only and headless EGL fails on the NVIDIA node.
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
WLR_RENDER_DRM_DEVICE=/dev/dri/renderD128 \
  sway -c "$OUT/sway.conf" > "$OUT/sway.log" 2>&1 &
SWAY_PID=$!

for _ in $(seq 1 40); do
  ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | sort > "$OUT/sockets-after"
  comm -13 "$OUT/sockets-before" "$OUT/sockets-after" | grep -qv '\.lock$' && break
  sleep 0.25
done
DISPLAY_NAME=$(basename "$(comm -13 "$OUT/sockets-before" "$OUT/sockets-after" |
  grep -v '\.lock$' | head -1)")

# Without this guard an empty display name lets GTK fall back and open the
# window in the operator's live session, where it steals focus and swallows the
# synthetic keystrokes.
if [ -z "$DISPLAY_NAME" ]; then
  echo "FATAL: no nested wayland socket appeared; not launching fm"
  kill "$SWAY_PID" 2>/dev/null
  exit 1
fi
echo "display: $DISPLAY_NAME"

if [ -n "${SEED_CLIPBOARD:-}" ]; then
  printf '%s' "$SEED_CLIPBOARD" |
    WAYLAND_DISPLAY="$DISPLAY_NAME" wl-copy --type x-special/gnome-copied-files
fi

RUST_BACKTRACE=1 env -u DISPLAY GDK_BACKEND=wayland WAYLAND_DISPLAY="$DISPLAY_NAME" \
  dbus-run-session -- "$REPO/target/debug/fm" "$START_DIR" > "$OUT/fm.log" 2>&1 &
FM_PID=$!

for _ in $(seq 1 40); do
  WAYLAND_DISPLAY="$DISPLAY_NAME" grim "$OUT/$SHOT.png" 2>/dev/null &&
    [ "$(stat -c%s "$OUT/$SHOT.png")" -gt 20000 ] && break
  sleep 0.5
done

if [ -n "$KEYS" ]; then
  WAYLAND_DISPLAY="$DISPLAY_NAME" wtype -s 600 -d 250 "$KEYS"
  sleep 1.5
  WAYLAND_DISPLAY="$DISPLAY_NAME" grim "$OUT/$SHOT.png"
fi

# Deliberately unquoted: this carries wtype flags such as `-k space -M ctrl -k c
# -m ctrl`. One wtype invocation per sequence — separate calls race on the
# virtual keyboard and silently drop keys.
if [ -n "${KEYS_ARGS:-}" ]; then
  WAYLAND_DISPLAY="$DISPLAY_NAME" wtype -s 600 -d 300 $KEYS_ARGS
  sleep 2.5
  WAYLAND_DISPLAY="$DISPLAY_NAME" grim "$OUT/$SHOT.png"
fi

if [ -n "${AFTER_CMD:-}" ]; then
  WAYLAND_DISPLAY="$DISPLAY_NAME" bash -c "$AFTER_CMD"
fi

grep -i "panicked" "$OUT/fm.log" && echo "PANIC DETECTED" || echo "no panics"

# Kill by PID, children first. Never `pkill -f`: with -f the pattern matches
# every command line including this script's own, so it kills the caller too.
kill "$FM_PID" 2>/dev/null
kill "$SWAY_PID" 2>/dev/null
for _ in $(seq 1 20); do
  [ -e "$XDG_RUNTIME_DIR/$DISPLAY_NAME" ] || break
  sleep 0.25
done
