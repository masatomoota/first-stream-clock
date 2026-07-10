#!/usr/bin/env bash
# appstore/sign_and_pkg.sh — turn ./dist/appstore/StreamClock.app (built by ../deploy.sh build)
# into a Mac App Store–ready, signed StreamClock.pkg.
#
#   ./deploy.sh build                 # produces dist/appstore/StreamClock.app (universal, unsigned)
#   bash appstore/sign_and_pkg.sh     # → appstore/build/StreamClock.pkg
#
# Env overrides:
#   BUILD_NUMBER   CFBundleVersion (must increase on every upload).  default: 1
#   SHORT_VERSION  CFBundleShortVersionString.                       default: 1.0.0
#   PROFILE_PATH   Mac App Store provisioning profile.               default: appstore/profiles/StreamClock_MAS.provisionprofile
#
# Recipe verified on this machine for net.firstcallmusic.zundachime (submitted 2026-07-06).
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
APPSTORE_DIR="$ROOT/appstore"
BUILD_DIR="$APPSTORE_DIR/build"
SRC_APP="$ROOT/dist/appstore/StreamClock.app"
APP="$BUILD_DIR/StreamClock.app"
PKG="$BUILD_DIR/StreamClock.pkg"

BUILD_NUMBER="${BUILD_NUMBER:-1}"
SHORT_VERSION="${SHORT_VERSION:-1.0.0}"
PROFILE_PATH="${PROFILE_PATH:-$APPSTORE_DIR/profiles/StreamClock_MAS.provisionprofile}"
ENTITLEMENTS="$ROOT/macos/StreamClock.entitlements"

SIGN_IDENTITY="Apple Distribution: masatomo ota (HS63RU33N9)"
INSTALLER_IDENTITY="3rd Party Mac Developer Installer: masatomo ota (HS63RU33N9)"
COPYRIGHT="© 2026 masatomo ota"

plist() { /usr/libexec/PlistBuddy -c "$1" "$APP/Contents/Info.plist"; }
set_key() { # set_key <key> <type> <value>
  plist "Add :$1 $2 $3" 2>/dev/null || plist "Set :$1 $3"
}

[ -d "$SRC_APP" ] || { echo "!! $SRC_APP missing — run ./deploy.sh build first" >&2; exit 1; }
[ -f "$PROFILE_PATH" ] || { echo "!! provisioning profile missing: $PROFILE_PATH" >&2; exit 1; }
[ -f "$ENTITLEMENTS" ] || { echo "!! entitlements missing: $ENTITLEMENTS" >&2; exit 1; }

echo "==> staging $APP"
rm -rf "$BUILD_DIR"; mkdir -p "$BUILD_DIR"
cp -R "$SRC_APP" "$APP"
# cargo-bundle copies the source tree's xattrs; codesign refuses those.
xattr -cr "$APP"

echo "==> Info.plist keys required by Mac App Store submission"
set_key CFBundleVersion              string  "$BUILD_NUMBER"
set_key CFBundleShortVersionString   string  "$SHORT_VERSION"
set_key LSMinimumSystemVersion       string  "11.0"
set_key LSApplicationCategoryType    string  "public.app-category.utilities"
set_key NSHighResolutionCapable      bool    true
set_key NSHumanReadableCopyright     string  "$COPYRIGHT"
plist "Add :ITSAppUsesNonExemptEncryption bool false" 2>/dev/null \
  || plist "Set :ITSAppUsesNonExemptEncryption false"
# The App Store build never captures audio; a stray usage string invites review questions.
plist "Delete :NSMicrophoneUsageDescription" 2>/dev/null || true

echo "==> embedding provisioning profile"
cp "$PROFILE_PATH" "$APP/Contents/embedded.provisionprofile"

echo "==> lipo -archs (must be universal)"
lipo -archs "$APP/Contents/MacOS/stream-clock"

echo "==> codesign (Apple Distribution + sandbox entitlements)"
codesign --force --sign "$SIGN_IDENTITY" \
  --entitlements "$ENTITLEMENTS" \
  --timestamp \
  "$APP"

echo "==> codesign --verify --deep --strict"
codesign --verify --deep --strict --verbose=2 "$APP"
echo "==> entitlements actually embedded:"
codesign -d --entitlements :- "$APP" 2>/dev/null

echo "==> productbuild → $PKG"
rm -f "$PKG"
productbuild --component "$APP" /Applications --sign "$INSTALLER_IDENTITY" "$PKG"

echo "==> pkgutil --check-signature"
pkgutil --check-signature "$PKG"

echo
echo "==> DONE"
echo "    app: $APP"
echo "    pkg: $PKG   (CFBundleVersion=$BUILD_NUMBER, CFBundleShortVersionString=$SHORT_VERSION)"
echo
echo "Next (key id / issuer id come from appstore/asc/credentials.env, which is gitignored):"
echo "  set -a; . appstore/asc/credentials.env; set +a"
echo "  xcrun altool --validate-app --type macos -f \"$PKG\" --apiKey \"\$ASC_KEY_ID\" --apiIssuer \"\$ASC_ISSUER_ID\""
echo "  xcrun altool --upload-app   --type macos -f \"$PKG\" --apiKey \"\$ASC_KEY_ID\" --apiIssuer \"\$ASC_ISSUER_ID\""
