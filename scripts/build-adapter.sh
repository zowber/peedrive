#!/bin/sh
# Build the SDK helper with an x-pm-appversion that identifies this exact build, as
# Proton's SDK guidelines require: external-drive-peedrive@{semver}-{channel}+{build}.{commit}
set -eu
cd "$(dirname "$0")/.."
semver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
package=$(sed -n 's/^Version: //p' scripts/package-deb.sh)      # e.g. 0.1.0~alpha7
channel=$(echo "$package" | sed -n 's/.*~\([a-z]*\)[0-9]*$/\1/p')
build=$(echo "$package" | sed -n 's/.*~[a-z]*\([0-9]*\)$/\1/p')
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- . ':!dist' || commit="$commit.dirty"
version="external-drive-peedrive@$semver${channel:+-$channel}+${build:+$build.}$commit"
echo "x-pm-appversion: $version"
define="PEEDRIVE_APP_VERSION=\"$version\""
bun build --compile --target=bun-linux-x64-baseline --define "$define" adapter/src/main.ts --outfile dist/peedrive-adapter
bun build --target=bun --define "$define" adapter/src/main.ts --outfile dist/peedrive-adapter.js
