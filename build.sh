#!/bin/sh
set -eu
cd "$(dirname "$0")"
app_name="everythingmac"

if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'build.sh requires macOS to create an .app bundle.' >&2
    exit 1
fi

package=$(rustup run stable cargo pkgid --locked)
version=${package##*@}
rustup run stable cargo build --locked --release --target-dir target

bundle="target/release/Everything Mac.app"
identifier="local.$(printf '%s' "$app_name" | tr '_' '-')"
mkdir -p "$bundle/Contents/MacOS"
cp "target/release/$app_name" "$bundle/Contents/MacOS/$app_name.new"
mv "$bundle/Contents/MacOS/$app_name.new" "$bundle/Contents/MacOS/$app_name"
# The executable's old spelling carries an obsolete bundle signature.
rm -f "$bundle/Contents/MacOS/eveythingmac"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Everything Mac</string>
<key>CFBundleDisplayName</key><string>Everything Mac</string>
<key>CFBundleIdentifier</key><string>$identifier</string>
<key>CFBundleExecutable</key><string>$app_name</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSDocumentsFolderUsageDescription</key><string>Index filenames and metadata in Documents so you can find your files.</string>
<key>NSDesktopFolderUsageDescription</key><string>Index filenames and metadata on your Desktop so you can find your files.</string>
<key>NSDownloadsFolderUsageDescription</key><string>Index filenames and metadata in Downloads so you can find your files.</string>
<key>NSRemovableVolumesUsageDescription</key><string>Search filenames on volumes you add to indexed folders.</string>
<key>NSAppleEventsUsageDescription</key><string>Show file information in Finder when you choose Get Info.</string>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
plutil -lint "$bundle/Contents/Info.plist"
codesign --force --sign - --timestamp=none "$bundle"
codesign --verify --deep --strict "$bundle"
printf 'Built %s\nOpen it with: open "%s"\n' "$bundle" "$bundle"
