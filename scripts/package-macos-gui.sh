#!/usr/bin/env bash
set -euo pipefail

target="${1:?target triple is required}"
arch="${2:?architecture label is required}"
out_dir="${3:-dist}"

version="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
bin_path="target/${target}/release/hf-mount-gui"
raw_name="hf-mount-gui-${arch}-apple-darwin"
app_dir="${out_dir}/hf-mount.app"

if [ ! -x "$bin_path" ]; then
  echo "missing built GUI binary: $bin_path" >&2
  exit 1
fi

mkdir -p "$out_dir"
cp "$bin_path" "${out_dir}/${raw_name}"

rm -rf "$app_dir"
mkdir -p "$app_dir/Contents/MacOS"
cp "$bin_path" "$app_dir/Contents/MacOS/hf-mount-gui"

cat > "$app_dir/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>hf-mount-gui</string>
  <key>CFBundleIdentifier</key>
  <string>com.huggingface.hf-mount-gui</string>
  <key>CFBundleName</key>
  <string>hf-mount</string>
  <key>CFBundleDisplayName</key>
  <string>hf-mount</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleVersion</key>
  <string>${version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

chmod +x "${out_dir}/${raw_name}" "$app_dir/Contents/MacOS/hf-mount-gui"
(cd "$out_dir" && zip -qry "${raw_name}.app.zip" hf-mount.app)
rm -rf "$app_dir"
