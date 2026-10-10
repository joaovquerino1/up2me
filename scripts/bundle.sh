#!/bin/zsh
# Builds an optimised arm64 release and wraps it in up2me.app (ad-hoc signed).
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -d vendor/yt-dlp && -f vendor/ffmpeg/ffmpeg && -f vendor/ffprobe/ffprobe ]] || ./scripts/fetch-tools.sh
[[ -n "${UP2ME_NATIVE:-}" ]] && export RUSTFLAGS="-C target-cpu=native"
cargo build --release
APP=target/up2me.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"
cp target/aarch64-apple-darwin/release/up2me "$APP/Contents/MacOS/up2me"
# Embedded tools: yt-dlp (needs its _internal dir next to it), ffmpeg, ffprobe
cp -R vendor/yt-dlp "$APP/Contents/Resources/bin/yt-dlp"
cp vendor/ffmpeg/ffmpeg vendor/ffprobe/ffprobe "$APP/Contents/Resources/bin/"
chmod +x "$APP/Contents/Resources/bin/yt-dlp/yt-dlp_macos" "$APP/Contents/Resources/bin/ffmpeg" "$APP/Contents/Resources/bin/ffprobe"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>up2me</string>
  <key>CFBundleDisplayName</key><string>up2me</string>
  <key>CFBundleIdentifier</key><string>io.github.joaovquerino1.up2me</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleExecutable</key><string>up2me</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleLocalizations</key><array><string>pt-BR</string><string>en</string></array>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict></plist>
PLIST
# Icon: assets/icon-1024.png -> AppIcon.icns
ICONSET=target/AppIcon.iconset
rm -rf "$ICONSET"; mkdir -p "$ICONSET"
for sz in 16 32 128 256 512; do
  sips -z $sz $sz assets/icon-1024.png --out "$ICONSET/icon_${sz}x${sz}.png" >/dev/null
  sips -z $((sz*2)) $((sz*2)) assets/icon-1024.png --out "$ICONSET/icon_${sz}x${sz}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
# Ad-hoc sign embedded Mach-O files first, then the app itself.
find "$APP/Contents/Resources/bin" -type f \( -perm -u+x -o -name '*.so' -o -name '*.dylib' \) | while read -r f; do
  file "$f" | grep -q 'Mach-O' && codesign --force --sign - "$f" 2>/dev/null || true
done
codesign --force --sign - "$APP"
echo "Built $APP"
