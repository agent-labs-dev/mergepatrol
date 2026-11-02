#!/bin/zsh
# Builds dist/MergeLoop.app, ad-hoc signed, for Apple silicon.
set -e
cd "$(dirname "$0")"

MIN_MACOS=13.0
MACOSX_DEPLOYMENT_TARGET=$MIN_MACOS cargo build --release --target aarch64-apple-darwin
VERSION=${$(cargo pkgid)##*@}

APP=dist/MergeLoop.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/aarch64-apple-darwin/release/mergepatrol-tray "$APP/Contents/MacOS/mergepatrol-tray"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>MergeLoop</string>
  <key>CFBundleExecutable</key><string>mergepatrol-tray</string>
  <key>CFBundleIdentifier</key><string>ai.mergepatrol.tray</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>MergeLoop</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSArchitecturePriority</key><array><string>arm64</string></array>
  <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"

codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
echo "built $APP"
