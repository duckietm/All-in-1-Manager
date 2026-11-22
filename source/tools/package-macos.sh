#!/bin/sh
# Builds the Mac version on a Mac (the GitHub macos runner): one program for Apple silicon and Intel (lipo), put in
# "All-in-1 Manager.app" with its icon, signed ad hoc (Apple silicon runs only signed programs), and zipped.
#
#   source/tools/package-macos.sh [out]
#
# out/ then holds:
#   all-in-1-manager-macos.zip                the app, what people download
#   all-in-1-manager-macos-universal          the program inside it, what the update check downloads
#   all-in-1-manager-macos-universal.sha256   its SHA-256, checked before the update replaces the running program
set -eu

out=${1:-out}
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(python3 source/tools/version.py)
app="$out/All-in-1 Manager.app"

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release -p aio-app --target "$target"
done

rm -rf "$out"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$out/all-in-1-manager-macos-universal" \
  target/aarch64-apple-darwin/release/all-in-1-manager target/x86_64-apple-darwin/release/all-in-1-manager
lipo -info "$out/all-in-1-manager-macos-universal"
codesign --force --sign - "$out/all-in-1-manager-macos-universal"
cp "$out/all-in-1-manager-macos-universal" "$app/Contents/MacOS/all-in-1-manager"

# The icon, in every size macOS asks for.
iconset="$out/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size source/crates/app/assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double source/crates/app/assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>All-in-1 Manager</string>
    <key>CFBundleDisplayName</key><string>All-in-1 Manager</string>
    <key>CFBundleIdentifier</key><string>com.github.duckietm.all-in-1-manager</string>
    <key>CFBundleExecutable</key><string>all-in-1-manager</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
# ditto keeps what a zip of an app needs (permissions, the signature's extended attributes).
(cd "$out" && ditto -c -k --keepParent "All-in-1 Manager.app" all-in-1-manager-macos.zip)
rm -rf "$app"
shasum -a 256 "$out/all-in-1-manager-macos-universal" | cut -d' ' -f1 > "$out/all-in-1-manager-macos-universal.sha256"
ls -l "$out"
