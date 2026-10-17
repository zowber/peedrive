#!/bin/sh
# Build an Arch Linux package from the files scripts/build.sh produced.
set -eu
cd "$(dirname "$0")/.."
# The Debian version is the single source; pacman versions cannot contain "~".
version=$(sed -n 's/^Version: //p' scripts/package-deb.sh)
pkgver=$(echo "$version" | tr -d '~')
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p dist
cat > "$work/PKGBUILD" <<PKGBUILD
pkgname=peedrive
pkgver=$pkgver
pkgrel=1
pkgdesc='Native GNOME folder sync client for Proton Drive'
arch=(x86_64)
license=(MIT)
depends=(glibc gcc-libs 'gtk4>=4.16' 'libadwaita>=1.6' libsecret python python-gobject gtk3 libayatana-appindicator)
optdepends=('nautilus-python: GNOME Files emblems and menu'
            'gnome-shell-extension-appindicator: top-bar indicator')
options=(!strip !debug)

package() {
  sh "$PWD/scripts/stage.sh" "\$pkgdir"
}
PKGBUILD
repo=$PWD
cd "$work"
BUILDDIR="$work/build" PKGDEST="$repo/dist" SRCDEST="$work" makepkg --force --nodeps
