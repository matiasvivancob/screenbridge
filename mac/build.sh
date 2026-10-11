#!/bin/sh
# Builds build/ScreenBridge.app (ad-hoc signed) and build/ScreenBridge-<version>.dmg and -mac.zip.
# Needs Xcode Command Line Tools. Run on a Mac: sh mac/build.sh
#
# AV1 (SVT-AV1) and VP9 (libvpx) are optional: if Homebrew's libraries are present (brew install svt-av1 libvpx)
# they are compiled into the Apple Silicon slice and bundled into the app; otherwise the app builds with H.264/HEVC only.
set -e
cd "$(dirname "$0")"
VERSION=$(tr -d '[:space:]' < ../VERSION)
APP=build/ScreenBridge.app
BREW=/opt/homebrew
rm -rf build
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources" build/obj

SVT_LIB=$(ls "$BREW"/lib/libSvtAv1Enc.*.dylib 2>/dev/null | grep -E 'libSvtAv1Enc\.[0-9]+\.dylib$' | head -1 || true)
VPX_LIB=$(ls "$BREW"/lib/libvpx.*.dylib 2>/dev/null | grep -E 'libvpx\.[0-9]+\.dylib$' | head -1 || true)
SOFT=0
if [ -n "$SVT_LIB" ] && [ -n "$VPX_LIB" ] && [ -f "$BREW/include/svt-av1/EbSvtAv1Enc.h" ] && [ -f "$BREW/include/vpx/vpx_encoder.h" ]; then
  SOFT=1
fi

for ARCH in arm64 x86_64; do
  DEFS=""; LINK=""
  if [ "$ARCH" = arm64 ] && [ "$SOFT" = 1 ]; then
    DEFS="-DSB_HAVE_SOFTENC -I$BREW/include -DSB_SVT_LIB=\"$(basename "$SVT_LIB")\" -DSB_VPX_LIB=\"$(basename "$VPX_LIB")\""
    # No -l flags: the libraries are dlopen()ed from Contents/Frameworks, so the app still launches where they cannot load.
    LINK="-Xlinker -rpath -Xlinker @executable_path/../Frameworks"
  fi
  clang -c -O2 -mmacosx-version-min=13.0 -arch "$ARCH" $DEFS Sources/softenc.c -o "build/obj/softenc-$ARCH.o"
  swiftc -O -target "$ARCH-apple-macos13.0" -import-objc-header Sources/Bridge.h \
    Sources/*.swift "build/obj/softenc-$ARCH.o" $LINK -o "build/obj/ScreenBridge-$ARCH"
done
lipo -create build/obj/ScreenBridge-arm64 build/obj/ScreenBridge-x86_64 -output "$APP/Contents/MacOS/ScreenBridge"

if [ "$SOFT" = 1 ]; then
  for LIB in "$SVT_LIB" "$VPX_LIB"; do
    NAME=$(basename "$LIB")
    cp -L "$LIB" "$APP/Contents/Frameworks/$NAME"
    chmod u+w "$APP/Contents/Frameworks/$NAME"
    install_name_tool -id "@rpath/$NAME" "$APP/Contents/Frameworks/$NAME" 2>/dev/null
  done
  echo "software codecs: AV1 (SVT-AV1) and VP9 (libvpx) bundled, Apple Silicon only; they load only on a macOS new enough for the bundled libraries"
else
  echo "software codecs: not built (install with: brew install svt-av1 libvpx)"
fi
sed "s/__VERSION__/$VERSION/g" Info.plist > "$APP/Contents/Info.plist"

# App icon from the shared 1024px master.
ICONSET=build/AppIcon.iconset
mkdir -p "$ICONSET"
for S in 16 32 128 256 512; do
  sips -z $S $S ../assets/icon.png --out "$ICONSET/icon_${S}x${S}.png" >/dev/null
  sips -z $((S * 2)) $((S * 2)) ../assets/icon.png --out "$ICONSET/icon_${S}x${S}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

# Ad-hoc signature: enough to run locally; see docs/INSTALL.md for opening it on other Macs.
for F in "$APP"/Contents/Frameworks/*.dylib; do [ -f "$F" ] && codesign --force --sign - "$F"; done
codesign --force --sign - "$APP"

mkdir -p build/dmg
cp -R "$APP" build/dmg/
ln -s /Applications build/dmg/Applications
hdiutil create -quiet -volname "ScreenBridge" -srcfolder build/dmg -ov -format UDZO "build/ScreenBridge-$VERSION.dmg"
# The .app is itself portable; this zip is the same app without the disk-image step.
ditto -c -k --keepParent "$APP" "build/ScreenBridge-$VERSION-mac.zip"
echo "built mac/build/ScreenBridge.app, ScreenBridge-$VERSION.dmg and ScreenBridge-$VERSION-mac.zip"
