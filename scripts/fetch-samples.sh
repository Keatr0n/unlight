#!/usr/bin/env bash
# Downloads the raw test files to $UNLIGHT_SAMPLES and checks them against samples.sha256.
set -euo pipefail
dest="${UNLIGHT_SAMPLES:-$HOME/.cache/unlight-samples}"
root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$dest"

fetch() {
  local name="$1" id="$2" remote="$3"
  [ -s "$dest/$name" ] && return
  remote="${remote// /%20}"
  remote="${remote//:/%3A}"
  curl -fsSL --retry 3 -o "$dest/$name.part" "https://raw.pixls.us/getfile.php/$id/nice/$remote"
  mv "$dest/$name.part" "$dest/$name"
}

fetch sony-a7rm4.ARW 3480 "Sony - ILCE-7RM4 - 14bit 14bit compressed (3:2).ARW"
fetch canon-eosr.CR3 4515 "Canon - EOS R - 3:2.CR3"
fetch canon-m6ii-craw.CR3 3714 "Canon - EOS M6 Mark II - C-RAW (3:2).CR3"
fetch canon-5d4.CR2 980 "Canon - EOS 5D Mark IV - RAW (3:2).CR2"
fetch nikon-z6.NEF 3582 "Nikon - Z 6 - 14bit 14bit lossless compressed (3:2).NEF"
fetch nikon-z8-compressed.NEF 6616 "Nikon - Z 8 - 8bit 8bit compressed (3:2).NEF"
fetch fuji-xt4.RAF 3914 "Fujifilm - X-T4 - 14bit 14bit lossless compressed (3:2).RAF"
fetch pixel4xl.dng 3502 "Google - Pixel 4 XL - 16bit (4:3).dng"
fetch gh5.RW2 1517 "Panasonic - DC-GH5 - 4:3.RW2"
fetch hasselblad-h6d.fff 2425 "Hasselblad - H6D-100c - 16bit (4:3).fff"
fetch sony-a7c-iso12800.ARW 4145 "Sony - ILCE-7C - 14bit 14bit uncompressed (3:2).ARW"

(cd "$dest" && grep -v '^#' "$root/samples.sha256" | sha256sum --check --quiet)
echo "samples ready in $dest"
