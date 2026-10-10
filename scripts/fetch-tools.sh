#!/bin/zsh
# Downloads the tools that get embedded in up2me.app into vendor/ (git-ignored).
#  - yt-dlp_macos.zip : official yt-dlp release (self-contained, no Python needed), SHA-256 verified
#  - ffmpeg / ffprobe : static arm64 builds from ffmpeg.martin-riedl.de
set -euo pipefail
cd "$(dirname "$0")/.."
V=vendor
rm -rf "$V/yt-dlp" "$V/ffmpeg" "$V/ffprobe" "$V/dl"
mkdir -p "$V/yt-dlp" "$V/ffmpeg" "$V/ffprobe" "$V/dl"

BASE=https://github.com/yt-dlp/yt-dlp/releases/latest/download
curl -fL --progress-bar "$BASE/yt-dlp_macos.zip" -o "$V/dl/yt-dlp_macos.zip"
curl -fsL "$BASE/SHA2-256SUMS" -o "$V/dl/SHA2-256SUMS"
want=$(grep -E ' yt-dlp_macos\.zip$' "$V/dl/SHA2-256SUMS" | awk '{print $1}')
got=$(shasum -a 256 "$V/dl/yt-dlp_macos.zip" | awk '{print $1}')
[[ -n "$want" && "$want" == "$got" ]] || { echo "yt-dlp checksum mismatch ($want vs $got)"; exit 1; }
ditto -x -k "$V/dl/yt-dlp_macos.zip" "$V/yt-dlp"

for t in ffmpeg ffprobe; do
  curl -fL --progress-bar "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/$t.zip" -o "$V/dl/$t.zip"
  ditto -x -k "$V/dl/$t.zip" "$V/$t"
done
rm -rf "$V/dl"
echo "OK:"; ls -R "$V" | head -30
