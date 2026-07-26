#!/bin/bash
# Builds bubbleTranslate.app.
#
# The bundle is not cosmetic. macOS grants Accessibility permission to a code
# signature, not to a path, so a bare `cargo run` binary loses the grant on
# every rebuild and the app silently stops seeing selections. An ad-hoc
# signature over a stable bundle identifier keeps the grant across rebuilds.

set -euo pipefail
cd "$(dirname "$0")"

APP="bubbleTranslate.app"
# The Accessibility grant is keyed to this identifier, so changing it makes
# macOS treat the app as new and ask for permission again.
BUNDLE_ID="com.pelamx.bubbleTranslate"

cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/bubbleTranslate "$APP/Contents/MacOS/bubbleTranslate"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>bubbleTranslate</string>
    <key>CFBundleDisplayName</key>     <string>bubbleTranslate</string>
    <key>CFBundleIdentifier</key>      <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>      <string>bubbleTranslate</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>0.1.0</string>
    <key>CFBundleVersion</key>         <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>  <string>11.0</string>
    <key>NSHumanReadableCopyright</key> <string>by pelamx</string>
    <!-- Menu-bar-less background app: no Dock icon, and showing the bubble
         never pulls focus away from whatever is being read. -->
    <key>LSUIElement</key>             <true/>
    <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

# Ad-hoc signature. Re-signing with the same identifier preserves the TCC grant.
codesign --force --sign - --identifier "$BUNDLE_ID" "$APP"

echo
echo "Built $APP"
echo
echo "First run:"
echo "  open $APP"
echo "  then allow it in System Settings › Privacy & Security › Accessibility"
echo "  and relaunch (the event tap is installed at startup)."
