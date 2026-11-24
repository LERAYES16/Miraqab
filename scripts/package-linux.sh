#!/bin/sh
# Build dist/miraqab-<ver>-linux-<arch>.tar.gz and dist/miraqab_<ver>_<debarch>.deb from a built binary.
# usage: scripts/package-linux.sh path/to/miraqab
set -eu
BIN=${1:-target/release/miraqab}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VER=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
ARCH=$(uname -m)
case "$ARCH" in x86_64) DEB=amd64;; aarch64) DEB=arm64;; *) DEB=$ARCH;; esac
OUT="$ROOT/dist"; STAGE=$(mktemp -d); mkdir -p "$OUT"
trap 'rm -rf "$STAGE"' EXIT

# --- tarball (works on any distro; install.sh installs under ~/.local without root) ---
T="$STAGE/miraqab-$VER-linux-$ARCH"; mkdir -p "$T"
cp "$BIN" "$T/miraqab"; cp "$ROOT/README.md" "$ROOT/LICENSE" "$ROOT/scripts/install.sh" "$ROOT/packaging/miraqab.desktop" "$T/"
tar -C "$STAGE" -czf "$OUT/miraqab-$VER-linux-$ARCH.tar.gz" "miraqab-$VER-linux-$ARCH"

# --- .deb (Debian, Ubuntu, Linux Mint, ...) ---
D="$STAGE/deb"; mkdir -p "$D/DEBIAN" "$D/usr/bin" "$D/usr/share/applications" "$D/usr/share/doc/miraqab"
install -m755 "$BIN" "$D/usr/bin/miraqab"
install -m644 "$ROOT/packaging/miraqab.desktop" "$D/usr/share/applications/miraqab.desktop"
install -m644 "$ROOT/README.md" "$ROOT/LICENSE" "$D/usr/share/doc/miraqab/"
SIZE=$(du -sk "$D/usr" | cut -f1)
DEPS=ffmpeg
GV=$(objdump -T "$BIN" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1 || true)
[ -n "$GV" ] && DEPS="ffmpeg, libc6 (>= $GV)"
cat > "$D/DEBIAN/control" <<CTL
Package: miraqab
Version: $VER
Section: video
Priority: optional
Architecture: $DEB
Installed-Size: $SIZE
Depends: $DEPS
Recommends: pulseaudio-utils | pipewire-pulse
Maintainer: Miraqab contributors <noreply@example.invalid>
Description: Lightweight Arabic-first screen recorder built on FFmpeg
 Records a monitor, window or region with optional system audio and microphone.
 Control panel opens in the browser (Arabic, right-to-left); also usable from the terminal.
 X11 only in this version; Wayland-only sessions are refused with an explanation.
CTL
dpkg-deb --root-owner-group --build "$D" "$OUT/miraqab_${VER}_$DEB.deb" >/dev/null
echo "created:"; ls -l "$OUT"
