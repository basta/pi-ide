#!/bin/sh
# Build target/release/pide.app (macOS) from the release binary, so pide can
# be started from Spotlight, Finder or the Dock. Started that way it opens the
# last project. Copy the bundle to /Applications if you like.
set -eu
cd "$(dirname "$0")/.."
cargo build -p pide --release
version=$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"name":"pide","version":"\([^"]*\)".*/\1/p')
app=target/release/pide.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/pide "$app/Contents/MacOS/pide"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>pide</string>
  <key>CFBundleDisplayName</key><string>pide</string>
  <key>CFBundleIdentifier</key><string>local.pide</string>
  <key>CFBundleExecutable</key><string>pide</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version:-0.1.0}</string>
  <key>CFBundleVersion</key><string>${version:-0.1.0}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
EOF
echo "built $app"
