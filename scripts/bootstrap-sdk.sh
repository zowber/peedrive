#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
revision=8ffae007b0fbb8bcef53514719d6f093bb32dcf9
if [ ! -d vendor/proton-sdk/.git ]; then
  git clone https://github.com/ProtonDriveApps/sdk.git vendor/proton-sdk
fi
git -C vendor/proton-sdk checkout --detach "$revision"
bun install --frozen-lockfile
