#!/bin/sh
# Builds build/ScreenBridge.app (universal, ad-hoc signed) and build/ScreenBridge-<version>.dmg.
# Needs Xcode Command Line Tools. Run on a Mac: sh mac/build.sh
set -e
cd "$(dirname "$0")"
VERSION=$(tr -d '[:space:]' < ../VERSION)
APP=build/ScreenBridge.app
rm -rf build
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" build/obj

for ARCH in arm64 x86_64; do
  swiftc -O -target "$ARCH-apple-macos13.0" -import-objc-header Sources/CGVirtualDisplay.h \
    Sources/*.swift -o "build/obj/ScreenBridge-$ARCH"
done
lipo -create build/obj/ScreenBridge-arm64 build/obj/ScreenBridge-x86_64 -output "$APP/Contents/MacOS/ScreenBridge"
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
codesign --force --sign - "$APP"

mkdir -p build/dmg
cp -R "$APP" build/dmg/
ln -s /Applications build/dmg/Applications
hdiutil create -quiet -volname "ScreenBridge" -srcfolder build/dmg -ov -format UDZO "build/ScreenBridge-$VERSION.dmg"
# The .app is itself portable; this zip is the same app without the disk-image step.
ditto -c -k --keepParent "$APP" "build/ScreenBridge-$VERSION-mac.zip"
echo "built mac/build/ScreenBridge.app, ScreenBridge-$VERSION.dmg and ScreenBridge-$VERSION-mac.zip"
