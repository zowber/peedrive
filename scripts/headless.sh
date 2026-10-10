#!/bin/sh
# Run a command against a private headless GTK display (Xvfb when available, else Broadway).
set -eu
if command -v xvfb-run >/dev/null; then
  exec xvfb-run -a -s '-screen 0 1280x800x24' env GDK_BACKEND=x11 "$@"
fi
display=$((50 + $$ % 200))
while :; do
  gtk4-broadwayd ":$display" >/dev/null 2>&1 &
  broadway=$!
  sleep 0.5
  # A busy display number makes broadwayd exit at once; try the next one.
  kill -0 "$broadway" 2>/dev/null && break
  display=$((display + 1))
done
trap 'kill $broadway 2>/dev/null || true' EXIT
GDK_BACKEND=broadway BROADWAY_DISPLAY=":$display" "$@"
