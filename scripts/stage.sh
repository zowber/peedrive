#!/bin/sh
# Install the built files under a package root, shared by the Debian and Arch packages.
set -eu
umask 022
root=$1
cd "$(dirname "$0")/.."
python3 scripts/collect-notices.py
install -Dm755 target/release/peedrive "$root/usr/bin/peedrive"
install -Dm755 target/release/peedrive-app "$root/usr/bin/peedrive-app"
install -Dm755 dist/peedrive-adapter "$root/usr/lib/peedrive/peedrive-adapter"
install -Dm644 dist/peedrive-adapter.js "$root/usr/lib/peedrive/peedrive-adapter.js"
install -Dm755 packaging/indicator.py "$root/usr/lib/peedrive/indicator.py"
install -Dm644 packaging/peedrive-nautilus.py "$root/usr/share/nautilus-python/extensions/peedrive.py"
install -Dm644 packaging/io.github.zowber.Peedrive.desktop "$root/usr/share/applications/io.github.zowber.Peedrive.desktop"
install -Dm644 packaging/io.github.zowber.Peedrive.svg "$root/usr/share/icons/hicolor/scalable/apps/io.github.zowber.Peedrive.svg"
install -Dm644 packaging/io.github.zowber.Peedrive.Sync.service "$root/usr/share/dbus-1/services/io.github.zowber.Peedrive.Sync.service"
install -Dm644 packaging/peedrive.service "$root/usr/lib/systemd/user/peedrive.service"
install -Dm644 packaging/peedrive-indicator.service "$root/usr/lib/systemd/user/peedrive-indicator.service"
install -Dm644 README.md "$root/usr/share/doc/peedrive/README.md"
install -Dm644 LICENSE "$root/usr/share/doc/peedrive/copyright"
install -Dm644 vendor/proton-sdk/LICENSE.md "$root/usr/share/doc/peedrive/Proton-SDK-LICENSE.md"
install -Dm644 THIRD_PARTY.md "$root/usr/share/doc/peedrive/THIRD_PARTY.md"
install -Dm644 packaging/Bun-LICENSE.md "$root/usr/share/doc/peedrive/Bun-LICENSE.md"
cp -R dist/dependency-notices "$root/usr/share/doc/peedrive/dependency-notices"
chmod -R go-w "$root/usr/share/doc/peedrive"
