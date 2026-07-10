#!/usr/bin/env bash
# deploy.sh — Build universal (arm64 + x86_64) macOS .app bundles for StreamClock
# in BOTH variants, then (optionally) install the FULL build to /Applications.
#
#   FULL     — Windows-equivalent: all time sources + LTC/MTC output. (default features)
#   APPSTORE — sandbox-safe: System + NTP + timecode OUTPUT (LTC/MTC).
#              (--no-default-features --features tc-out)
#
# Usage:
#   ./deploy.sh           # build + bundle + install FULL to /Applications  (full deploy)
#   ./deploy.sh all       #   (same as above)
#   ./deploy.sh build     # build + bundle both variants into ./dist (no install)
#   ./deploy.sh install   # install ./dist/full/StreamClock.app to /Applications
#
# Requirements: rustup (aarch64-apple-darwin + x86_64-apple-darwin targets),
#               cargo-bundle, Xcode Command Line Tools (lipo, PlistBuddy).
set -euo pipefail

cd "$(dirname "$0")"
ROOT="$(pwd)"
BIN="stream-clock"          # [package].name  → Contents/MacOS/<BIN>
APP="StreamClock.app"       # [package.metadata.bundle].name
DIST="$ROOT/dist"
LOG="$ROOT/build_log.txt"   # gitignored
ARM="aarch64-apple-darwin"
X86="x86_64-apple-darwin"
MIC_DESC="StreamClock decodes SMPTE LTC timecode from the selected audio input device."

# rustup toolchain + ~/.cargo/bin (cargo-bundle lives here regardless of installer)
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
export PATH="$HOME/.cargo/bin:$PATH"

log() { echo "==> $*" | tee -a "$LOG"; }

ensure_targets() {
  command -v rustup >/dev/null || { echo "ERROR: rustup not installed"; exit 1; }
  command -v cargo-bundle >/dev/null || { echo "ERROR: cargo-bundle not installed (cargo install cargo-bundle)"; exit 1; }
  for t in "$ARM" "$X86"; do
    rustup target list --installed | grep -qx "$t" || rustup target add "$t"
  done
}

# build_variant <label> [extra cargo flags...]
build_variant() {
  local label="$1"; shift
  local extra=("$@")
  local out="$DIST/$label"
  log "[$label] cargo build --release ${extra[*]:-} ($ARM)"
  cargo build --release --target "$ARM" ${extra[@]+"${extra[@]}"} >>"$LOG" 2>&1
  log "[$label] cargo build --release ${extra[*]:-} ($X86)"
  cargo build --release --target "$X86" ${extra[@]+"${extra[@]}"} >>"$LOG" 2>&1
  log "[$label] cargo bundle --release ${extra[*]:-} ($ARM)"
  cargo bundle --release --target "$ARM" ${extra[@]+"${extra[@]}"} >>"$LOG" 2>&1

  local src; src="$(ls -d "$ROOT/target/$ARM/release/bundle/osx/"*.app 2>/dev/null | head -1)"
  [ -n "$src" ] && [ -d "$src" ] || { echo "ERROR: [$label] bundle .app not found under target/$ARM/release/bundle/osx/"; exit 1; }
  rm -rf "$out"; mkdir -p "$out"
  cp -R "$src" "$out/$APP"

  log "[$label] lipo → universal $BIN"
  lipo -create -output "$out/$APP/Contents/MacOS/$BIN" \
       "$ROOT/target/$ARM/release/$BIN" \
       "$ROOT/target/$X86/release/$BIN"
  lipo -info "$out/$APP/Contents/MacOS/$BIN" | tee -a "$LOG"
}

do_build() {
  ensure_targets
  : > "$LOG"
  log "toolchain: $(cargo --version)"
  mkdir -p "$DIST"

  # FULL (all sources). Needs mic usage string for LTC audio input or it crashes on use.
  build_variant full
  local plist="$DIST/full/$APP/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Add :NSMicrophoneUsageDescription string $MIC_DESC" "$plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Set :NSMicrophoneUsageDescription $MIC_DESC" "$plist"
  log "[full] Info.plist NSMicrophoneUsageDescription set"

  # APPSTORE (System + NTP sources + LTC/MTC output; no mic/capture). Entitlements at codesign.
  build_variant appstore --no-default-features --features tc-out
  cp "$ROOT/macos/StreamClock.entitlements" "$DIST/appstore/StreamClock.entitlements"
  log "[appstore] entitlements staged next to .app (apply at codesign)"

  echo
  log "BUILD COMPLETE"
  echo "  FULL     : $DIST/full/$APP"
  echo "  APPSTORE : $DIST/appstore/$APP   (unsigned; + StreamClock.entitlements)"
}

do_install() {
  local app="$DIST/full/$APP"
  [ -d "$app" ] || { echo "ERROR: $app missing — run './deploy.sh build' first"; exit 1; }
  local dest="/Applications/$APP"
  if rm -rf "$dest" 2>/dev/null && cp -R "$app" "$dest" 2>/dev/null; then
    log "installed FULL → $dest"
  else
    dest="$HOME/Applications/$APP"
    mkdir -p "$HOME/Applications"
    rm -rf "$dest"; cp -R "$app" "$dest"
    log "installed FULL → $dest (fell back to ~/Applications)"
  fi
  echo "  Launch: open \"$dest\""
}

case "${1:-all}" in
  build)   do_build ;;
  install) do_install ;;
  all)     do_build; do_install ;;
  *) echo "usage: $0 [all|build|install]"; exit 2 ;;
esac
