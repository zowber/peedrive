#!/bin/sh
set -eu
umask 022
cd "$(dirname "$0")/.."
stage=$(mktemp -d)
chmod 755 "$stage"
trap 'rm -rf "$stage"' EXIT
sh scripts/stage.sh "$stage"
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
