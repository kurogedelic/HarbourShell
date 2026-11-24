#!/bin/sh
# Build harbour-shell_<version>_arm64.deb (run ./tools_build_arm64.sh first).
# dpkg-deb runs in Docker; sources go in through stdin, the .deb comes out on stdout.
set -eu
# macOS tar would add AppleDouble "._*" entries for files with extended attributes.
export COPYFILE_DISABLE=1
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# A build that is not the tagged release gets a later Debian version (0.1.0+git202611200135.abc1234),
# so apt / dpkg install it over the release or an older dev build instead of saying "already newest".
# HARBOUR_RELEASE=1 forces the plain version.
if [ "${HARBOUR_RELEASE:-0}" != 1 ] && ! git describe --exact-match --tags "v$VERSION" >/dev/null 2>&1; then
  VERSION="$VERSION+git$(date -u +%Y%m%d%H%M).$(git rev-parse --short HEAD 2>/dev/null || echo local)"
fi
# Apps that are not ready for a release: left out of a release package (they stay in the +git
# builds). HARBOUR_RELEASE_HIDE overrides the list.
RELEASE_HIDE=""
case "$VERSION" in *+git*) ;; *) RELEASE_HIDE="${HARBOUR_RELEASE_HIDE-yacht}" ;; esac
BIN=dist/arm64/harbour-shell
[ -x "$BIN" ] || { echo "build the binary first: ./tools_build_arm64.sh" >&2; exit 1; }
SPLASH=dist/bundle/boot/splash.bmp
[ -f "$SPLASH" ] || { echo "make the bundle first (it draws the boot picture): ./tools_make_bundle.sh" >&2; exit 1; }
OUT=dist/arm64/harbour-shell_${VERSION}_arm64.deb
tar cf - --exclude=assets/icons-src --exclude=assets/statusbar-src --exclude=.DS_Store --exclude="._*" --exclude="assets/images/icons/Test*.png" "$BIN" dist/arm64/harbour_ime_dict "$SPLASH" assets apps packaging LICENSE NOTICE THIRD_PARTY.md THIRD_PARTY_LICENSES.md |
docker run -i --rm rust:trixie sh -c '
  set -eu
  mkdir /work && tar xf - -C /work && cd /work
  VERSION='"$VERSION"'
  R=/pkg
  mkdir -p $R/DEBIAN $R/usr/share/harbour-shell/bin $R/usr/lib/systemd/user $R/usr/bin
  install -m755 dist/arm64/harbour-shell $R/usr/share/harbour-shell/bin/harbour-shell
  install -m755 dist/arm64/harbour_ime_dict $R/usr/share/harbour-shell/bin/harbour_ime_dict
  install -m755 packaging/ime_install.sh $R/usr/share/harbour-shell/ime_install.sh
  # One-time boot setup (sudo sh /usr/share/harbour-shell/install_system.sh): release the boot
  # splash once Harbour runs, and show the Harbour boot picture.
  install -m755 packaging/install_system.sh $R/usr/share/harbour-shell/install_system.sh
  install -m644 packaging/harbour-splash-release.service packaging/fb_load-harbour.conf $R/usr/share/harbour-shell/
  install -D -m644 dist/bundle/boot/splash.bmp $R/usr/share/harbour-shell/boot/splash.bmp
  cp -r assets $R/usr/share/harbour-shell/assets
  cp -r apps $R/usr/share/harbour-shell/apps
  for a in '"$RELEASE_HIDE"'; do rm -rf "$R/usr/share/harbour-shell/apps/$a"; done
  mkdir -p $R/usr/share/doc/harbour-shell
  cp LICENSE NOTICE THIRD_PARTY.md THIRD_PARTY_LICENSES.md $R/usr/share/doc/harbour-shell/
  install -m644 packaging/harbour-shell.service $R/usr/lib/systemd/user/harbour-shell.service
  install -m755 packaging/harbourctl $R/usr/bin/harbourctl
  cat > $R/DEBIAN/control <<CTL
Package: harbour-shell
Version: $VERSION
Section: utils
Priority: optional
Architecture: arm64
Depends: libsdl2-2.0-0, libsdl2-image-2.0-0
Replaces: harbour-launcher
Conflicts: harbour-launcher
Recommends: network-manager, pipewire, wireplumber, bluez
Maintainer: kurogedelic
Description: Harbour Shell for the M5Stack Cardputer Zero
 A replacement home screen for the official APPLaunch. It lists the same
 .desktop apps, runs them, and offers its own Settings, Terminal and tools.
 The official launcher stays installed; use harbourctl to switch.
CTL
  install -m755 packaging/deb-postinst.sh $R/DEBIAN/postinst
  dpkg-deb --root-owner-group --build $R /tmp/out.deb >&2
  cat /tmp/out.deb
' > "$OUT.tmp"
mv "$OUT.tmp" "$OUT"
ls -la "$OUT"
