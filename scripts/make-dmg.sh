#!/bin/zsh
# Builds target/up2me-<version>-arm64.dmg (drag-to-Applications installer) from target/up2me.app.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -d target/up2me.app ]] || ./scripts/bundle.sh
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
DMG="target/up2me-${VERSION}-arm64.dmg"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
cp -R target/up2me.app "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "up2me" -srcfolder "$STAGE" -ov -format UDZO -imagekey zlib-level=9 "$DMG" >/dev/null
echo "Built $DMG"
shasum -a 256 "$DMG"
