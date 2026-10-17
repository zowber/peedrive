#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mkdir -p dist
export PATH="$HOME/.cargo/bin:$PATH"
export GSK_RENDERER=cairo
export PEEDRIVE_TEST_SCREENSHOTS="$PWD/dist"
dbus-run-session --config-file=tests/dbus-session.conf -- sh scripts/headless.sh \
  cargo test --locked --features gui --bin peedrive-app -- --ignored
# scene:page pairs rendered against the synthetic service.
for pair in setup:overview overview:overview syncing:overview folders:folders activity:activity issues:issues; do
  scene=${pair%%:*}
  export PEEDRIVE_UI_SCENE="$scene" PEEDRIVE_SCREENSHOT_PAGE="${pair#*:}"
  export PEEDRIVE_SCREENSHOT="$PWD/dist/peedrive-ui-$scene.png"
  rm -f "$PEEDRIVE_SCREENSHOT"
  # Headless Broadway occasionally loses the first frame; retry before failing.
  for attempt in 1 2 3; do
    dbus-run-session --config-file=tests/dbus-session.conf -- sh scripts/headless.sh sh -c '
      python3 -W ignore tests/ui-service.py > /dev/null &
      fixture_pid=$!
      trap "kill $fixture_pid 2>/dev/null || true" EXIT
      sleep 1
      target/debug/peedrive-app
    '
    test -s "$PEEDRIVE_SCREENSHOT" && break
  done
  test -s "$PEEDRIVE_SCREENSHOT"
done
cp dist/peedrive-ui-overview.png dist/peedrive-ui.png
