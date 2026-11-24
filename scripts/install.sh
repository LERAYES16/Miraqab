#!/bin/sh
# Per-user install, no root needed. Run from the extracted tarball.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
PREFIX=${PREFIX:-$HOME/.local}
mkdir -p "$PREFIX/bin" "$PREFIX/share/applications"
install -m755 "$HERE/miraqab" "$PREFIX/bin/miraqab"
sed "s|^Exec=.*|Exec=$PREFIX/bin/miraqab gui|" "$HERE/miraqab.desktop" > "$PREFIX/share/applications/miraqab.desktop"
echo "installed to $PREFIX/bin/miraqab"
command -v ffmpeg >/dev/null 2>&1 || echo "NOTE: ffmpeg was not found. Install it with your package manager (e.g. apt install ffmpeg, dnf install ffmpeg, pacman -S ffmpeg)."
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "NOTE: add $PREFIX/bin to your PATH.";; esac
