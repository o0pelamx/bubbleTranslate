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

# Sign with the self-signed certificate from ./setup-signing.sh when it exists.
#
# This is what keeps the Accessibility grant alive across rebuilds. An ad-hoc
# signature's designated requirement is the binary's own hash, so every build
# looks like a different program to macOS and the grant stops applying while
# still appearing enabled. A certificate pins the requirement to the identity
# instead, and later builds keep satisfying it.
SIGNING_KEYCHAIN="bubbletranslate-signing"
SIGNING_CERT="bubbleTranslate Signing"
KEYCHAIN_PW_FILE="$HOME/.config/bubbletranslate/signing.pw"

if security find-identity -p codesigning "$SIGNING_KEYCHAIN" 2>/dev/null | grep -q "$SIGNING_CERT"; then
    # Unlocking is a no-op when it is already open, and the build fails with an
    # opaque error if it is not.
    if [[ -f "$KEYCHAIN_PW_FILE" ]]; then
        security unlock-keychain -p "$(cat "$KEYCHAIN_PW_FILE")" "$SIGNING_KEYCHAIN" 2>/dev/null || true
    fi
    codesign --force --sign "$SIGNING_CERT" --keychain "$SIGNING_KEYCHAIN" \
        --identifier "$BUNDLE_ID" "$APP"
    echo "signed with: $SIGNING_CERT (Accessibility grant survives rebuilds)"
else
    codesign --force --sign - --identifier "$BUNDLE_ID" "$APP"
    echo "ad-hoc signed — run ./setup-signing.sh to stop re-granting Accessibility"
fi

echo
echo "Built $APP"
echo
echo "First run:"
echo "  open $APP"
echo "  then allow it in System Settings › Privacy & Security › Accessibility"
echo "  and relaunch (the event tap is installed at startup)."
