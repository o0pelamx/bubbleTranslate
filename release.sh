#!/bin/bash
# Builds bubbleTranslate.dmg — a drag-to-Applications installer.
#
# Works with no Apple developer account: the app is ad-hoc signed and the DMG
# installs fine, but Gatekeeper warns on first launch and the user has to
# clear it once via System Settings › Privacy & Security › Open Anyway.
#
# With an account, set both variables and the same script produces a release
# that opens with no warning at all:
#
#   SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
#   NOTARY_PROFILE=bubbleTranslate-notary \
#   ./release.sh
#
# The notary profile is created once with:
#   xcrun notarytool store-credentials bubbleTranslate-notary \
#     --apple-id you@example.com --team-id TEAMID --password <app-specific-password>

set -euo pipefail
cd "$(dirname "$0")"

APP="bubbleTranslate.app"
DMG="bubbleTranslate.dmg"
VOLNAME="bubbleTranslate"
STAGE="$(mktemp -d)/dmg"
trap 'rm -rf "$(dirname "$STAGE")"' EXIT

SIGN_IDENTITY="${SIGN_IDENTITY:-}"
NOTARY_PROFILE="${NOTARY_PROFILE:-}"

# --- build the .app ---------------------------------------------------------

./bundle.sh > /dev/null
echo "built $APP"

# --- sign -------------------------------------------------------------------

if [[ -n "$SIGN_IDENTITY" ]]; then
    # --options runtime enables the hardened runtime, which notarization
    # requires. It does not interfere with the Accessibility permission.
    codesign --force --deep --options runtime --timestamp \
        --sign "$SIGN_IDENTITY" "$APP"
    echo "signed with: $SIGN_IDENTITY"
else
    echo "no SIGN_IDENTITY — keeping the ad-hoc signature (Gatekeeper will warn)"
fi

# --- notarize ---------------------------------------------------------------
#
# Notarization runs on a zip of the app rather than the DMG, so the ticket can
# be stapled into the app before the DMG is built around it.

if [[ -n "$NOTARY_PROFILE" ]]; then
    if [[ -z "$SIGN_IDENTITY" ]]; then
        echo "error: notarization needs a Developer ID signature; set SIGN_IDENTITY" >&2
        exit 1
    fi
    ZIP="$(dirname "$STAGE")/bubbleTranslate.zip"
    ditto -c -k --keepParent "$APP" "$ZIP"
    xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP"
    echo "notarized and stapled"
fi

# --- lay out the installer --------------------------------------------------

mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
# The symlink is what makes the window a drag-to-install target.
ln -s /Applications "$STAGE/Applications"

cat > "$STAGE/Read me first.txt" <<'TXT'
bubbleTranslate — by pelamx

1. Drag bubbleTranslate.app onto the Applications folder.

2. Launch it. macOS blocks the first launch and says it "could not verify"
   the app — this app is not notarized by Apple. Dismiss that, then open
   System Settings > Privacy & Security, scroll to Security, and click
   "Open Anyway". This is only needed once.

   (Control-click > Open does not work for this on macOS 15 and later.)

3. macOS will ask for Accessibility permission; grant it in
   System Settings > Privacy & Security > Accessibility.

4. QUIT AND RELAUNCH. The permission only takes effect on a fresh start,
   because the event tap is installed when the app launches.

Then select text anywhere — double-click a word, drag a phrase, triple-click
a line — and the translation appears at your cursor.

The app has no Dock icon while it runs in the background. Look for the globe
in the menu bar to reopen the window or quit.
TXT

# --- build the disk image ---------------------------------------------------

rm -f "$DMG"
hdiutil create \
    -volname "$VOLNAME" \
    -srcfolder "$STAGE" \
    -ov -format UDZO \
    "$DMG" > /dev/null

if [[ -n "$SIGN_IDENTITY" ]]; then
    codesign --force --sign "$SIGN_IDENTITY" "$DMG"
    # Verifies the DMG the way Gatekeeper will on the user's machine.
    spctl -a -vvv -t install "$DMG" || true
fi

echo
echo "built $DMG ($(du -h "$DMG" | cut -f1))"
if [[ -z "$NOTARY_PROFILE" ]]; then
    echo
    echo "Not notarized. On another Mac the first launch is blocked; clear it once"
    echo "via System Settings > Privacy & Security > Open Anyway. See this script's"
    echo "header for the signed, warning-free release path."
fi
