#!/bin/bash
# Creates the self-signed certificate that bundle.sh signs with.
#
# Why this exists: an ad-hoc signature (codesign --sign -) has no stable
# identity. Its designated requirement is the binary's own hash:
#
#     designated => cdhash H"e39883dc..."
#
# so every rebuild produces a program macOS considers unrelated to the one the
# user granted Accessibility to. The switch in System Settings keeps showing
# "on" while the grant silently stops applying — the permission has to be
# removed and re-granted after every single build.
#
# Signing with a certificate instead pins the requirement to the certificate:
#
#     designated => identifier "com.pelamx.bubbleTranslate"
#                   and certificate root = H"c0a427ea..."
#
# which every later build still satisfies, so the grant survives rebuilds.
# The certificate is self-signed, which is enough for this: Gatekeeper still
# warns on download exactly as it did before (that needs a paid Developer ID),
# but TCC only cares that the identity is stable.
#
# Run once. Re-running is a no-op unless the keychain is missing, because a new
# certificate would be a new identity and would cost another re-grant.

set -euo pipefail

KEYCHAIN="bubbletranslate-signing"
CERT_NAME="bubbleTranslate Signing"
CONFIG_DIR="$HOME/.config/bubbletranslate"
PW_FILE="$CONFIG_DIR/signing.pw"
P12_FILE="$CONFIG_DIR/signing.p12"
PEM_FILE="$CONFIG_DIR/signing.pem"

# The private key lives here, outside the repository. Never commit these.
mkdir -p "$CONFIG_DIR"
chmod 700 "$CONFIG_DIR"

if security find-identity -p codesigning "$KEYCHAIN" 2>/dev/null | grep -q "$CERT_NAME"; then
    echo "signing identity already present: $CERT_NAME"
    echo "nothing to do — creating a new one would cost another Accessibility re-grant."
    exit 0
fi

echo "creating signing certificate..."

PW="$(openssl rand -base64 24)"
printf '%s\n' "$PW" > "$PW_FILE"
chmod 600 "$PW_FILE"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# codeSigning EKU is what makes codesign accept this as a signing identity.
openssl req -x509 -newkey rsa:2048 -keyout "$WORK/key.pem" -out "$WORK/cert.pem" \
    -days 3650 -nodes \
    -subj "/CN=$CERT_NAME/O=pelamx" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature" 2>/dev/null

openssl pkcs12 -export -out "$WORK/cert.p12" \
    -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
    -passout "pass:$PW" -name "$CERT_NAME"

# A dedicated keychain with a password we know, so builds never stop for a
# GUI prompt the way the login keychain would.
security delete-keychain "$KEYCHAIN" 2>/dev/null || true
security create-keychain -p "$PW" "$KEYCHAIN"
# No auto-lock: a locked keychain mid-build fails with a confusing error.
security set-keychain-settings "$KEYCHAIN"
security unlock-keychain -p "$PW" "$KEYCHAIN"
security import "$WORK/cert.p12" -k "$KEYCHAIN" -P "$PW" -T /usr/bin/codesign -A
# Without this, codesign triggers the "wants to use a key" dialog on every run.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" "$KEYCHAIN" > /dev/null

# Keep the keychain in the user's search list alongside whatever is already there.
EXISTING="$(security list-keychains -d user | sed 's/"//g' | xargs)"
# shellcheck disable=SC2086
security list-keychains -d user -s $EXISTING "$KEYCHAIN"

cp "$WORK/cert.p12" "$P12_FILE"
cp "$WORK/cert.pem" "$PEM_FILE"
chmod 600 "$P12_FILE" "$PEM_FILE"

echo
echo "created identity: $CERT_NAME"
security find-identity -p codesigning "$KEYCHAIN" | grep "$CERT_NAME" || true
echo
echo "Private key and password: $CONFIG_DIR (outside the repo, mode 600)."
echo "Back that directory up — losing it means a new certificate, which means"
echo "granting Accessibility again."
echo
echo "Next: ./bundle.sh, install, and grant Accessibility one final time."
