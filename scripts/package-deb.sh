#!/bin/sh
set -eu
umask 022
cd "$(dirname "$0")/.."
stage=$(mktemp -d)
chmod 755 "$stage"
trap 'rm -rf "$stage"' EXIT
python3 scripts/collect-notices.py
install -Dm755 target/release/peedrive "$stage/usr/bin/peedrive"
install -Dm755 target/release/peedrive-app "$stage/usr/bin/peedrive-app"
install -Dm755 dist/peedrive-adapter "$stage/usr/lib/peedrive/peedrive-adapter"
install -Dm644 dist/peedrive-adapter.js "$stage/usr/lib/peedrive/peedrive-adapter.js"
install -Dm755 packaging/indicator.py "$stage/usr/lib/peedrive/indicator.py"
install -Dm644 packaging/peedrive-nautilus.py "$stage/usr/share/nautilus-python/extensions/peedrive.py"
install -Dm644 packaging/io.github.zowber.Peedrive.desktop "$stage/usr/share/applications/io.github.zowber.Peedrive.desktop"
install -Dm644 packaging/io.github.zowber.Peedrive.svg "$stage/usr/share/icons/hicolor/scalable/apps/io.github.zowber.Peedrive.svg"
install -Dm644 packaging/io.github.zowber.Peedrive.Sync.service "$stage/usr/share/dbus-1/services/io.github.zowber.Peedrive.Sync.service"
install -Dm644 packaging/peedrive.service "$stage/usr/lib/systemd/user/peedrive.service"
install -Dm644 packaging/peedrive-indicator.service "$stage/usr/lib/systemd/user/peedrive-indicator.service"
install -Dm644 README.md "$stage/usr/share/doc/peedrive/README.md"
install -Dm644 LICENSE "$stage/usr/share/doc/peedrive/copyright"
install -Dm644 vendor/proton-sdk/LICENSE.md "$stage/usr/share/doc/peedrive/Proton-SDK-LICENSE.md"
install -Dm644 THIRD_PARTY.md "$stage/usr/share/doc/peedrive/THIRD_PARTY.md"
install -Dm644 packaging/Bun-LICENSE.md "$stage/usr/share/doc/peedrive/Bun-LICENSE.md"
cp -R dist/dependency-notices "$stage/usr/share/doc/peedrive/dependency-notices"
chmod -R go-w "$stage/usr/share/doc/peedrive"
mkdir -p "$stage/DEBIAN" dist
cat > "$stage/DEBIAN/control" <<'EOF'
Package: peedrive
Version: 0.1.0~alpha9
Section: net
Priority: optional
Architecture: amd64
Maintainer: Peedrive contributors <peedrive@localhost>
Depends: libc6 (>= 2.39), libgtk-4-1 (>= 4.16), libadwaita-1-0 (>= 1.6), libsecret-1-0, python3, python3-gi, gir1.2-gtk-3.0, gir1.2-ayatanaappindicator3-0.1
Recommends: python3-nautilus
Description: Native GNOME folder sync client for Proton Drive
 Unofficial personal-use alpha client with local folder mappings,
 conflict preservation, GNOME Keyring sessions and a background sync service.
EOF
dpkg-deb --root-owner-group --build "$stage" dist/peedrive_0.1.0~alpha9_amd64.deb
