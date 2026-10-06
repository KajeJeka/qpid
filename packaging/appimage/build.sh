#!/usr/bin/env bash
# q-pid AppImage (linux-port.md section 11). Run from the repo root inside
# an old-glibc container (ubuntu:22.04 = oldest LTS still receiving
# security updates as of this plan; glibc 2.35 baseline). Needs rustc,
# pkg-config, libasound2-dev, libfontconfig1-dev, file (appimagetool). Produces qpid-*.AppImage in the repo root.
set -euo pipefail

ARCH="$(uname -m)"    # x86_64 | aarch64
cargo build --release --locked

rm -rf AppDir
mkdir -p AppDir/usr/bin
cp target/release/qpid AppDir/usr/bin/qpid

curl -fsSL -o linuxdeploy \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${ARCH}.AppImage"
chmod +x linuxdeploy

# CI containers have no FUSE: extract-and-run instead of mounting.
export APPIMAGE_EXTRACT_AND_RUN=1

# linuxdeploy keeps the file's basename, but the desktop entry says
# Icon=qpid — deploy the icon under that name or resolution fails.
ICON_NAME_DIR="$(mktemp -d)"
cp assets/icon.png "$ICON_NAME_DIR/qpid.png"

./linuxdeploy \
  --appdir AppDir \
  --executable target/release/qpid \
  --desktop-file assets/qpid.desktop \
  --icon-file "$ICON_NAME_DIR/qpid.png" \
  --output appimage
