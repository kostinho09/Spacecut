#!/bin/bash
# Builds Spacecut for the Mac and zips it for a release, with nothing of the
# computer it was built on inside:
#   - Rust keeps the file path of each place that can stop with an error (for
#     its messages), and for the libraries that path is under the home
#     folder; here the home folder is written as "~" instead.
#   - The zip is made without .DS_Store (Finder's folder notes) and without
#     __MACOSX (extra Mac file data), which a zip from Finder would carry.
# It stops, and makes no zip, if the app still holds a path from the home
# folder or the user's name.
#
# Use: scripts/release-mac.sh        → dist/Spacecut_v<version>-Mac.zip
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1)
short=${version%.0}

# Built apart from the everyday builds, so neither has to start over
export CARGO_TARGET_DIR="$PWD/src-tauri/target/release-clean"
export RUSTFLAGS="--remap-path-prefix=$HOME=~"
cargo tauri build

app="$CARGO_TARGET_DIR/release/bundle/macos/Spacecut.app"
[ -d "$app" ] || { echo "No app was built at $app"; exit 1; }

# Nothing personal in the app
found=$(grep -r -l -a -e "$HOME" -e "/Users/$USER/" "$app" || true)
if [ -n "$found" ]; then
  echo "Stopped: these files still hold a path from this computer:"
  echo "$found"
  exit 1
fi
find "$app" -name .DS_Store -delete

# The whole app signed ad hoc (no Apple account): the build signs only the
# program inside, which macOS takes for a damaged app once it is downloaded
# ("can't be opened, move to the Bin") instead of offering Open Anyway
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app" || { echo "Stopped: the app's signature is not valid"; exit 1; }

mkdir -p dist
zip="dist/Spacecut_v$short-Mac.zip"
rm -f "$zip"
ditto -c -k --norsrc --noextattr --keepParent "$app" "$zip"

if zipinfo -1 "$zip" | grep -E '(^|/)(\.DS_Store|__MACOSX)'; then
  echo "Stopped: the zip holds Finder files"
  exit 1
fi
# The signature as it arrives: the app unzipped elsewhere
check=$(mktemp -d)
ditto -x -k "$zip" "$check"
codesign --verify --deep --strict "$check/Spacecut.app" || { echo "Stopped: the app in the zip has no valid signature"; rm -rf "$check"; exit 1; }
rm -rf "$check"
echo "Ready: $zip ($(du -h "$zip" | cut -f1)), checked: no personal paths, no Finder files, signature valid"
