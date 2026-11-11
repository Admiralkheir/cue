#!/usr/bin/env bash
# Builds Cue.app as a universal (Apple silicon + Intel) bundle and zips it.
#
#   scripts/package-macos.sh [output-dir]
#
# CUE_MAC_TARGETS overrides the architectures, e.g. CUE_MAC_TARGETS=aarch64-apple-darwin
# for a quicker single-architecture build.
set -euo pipefail

cd "$(dirname "$0")/.."
out="${1:-dist}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
read -r -a targets <<< "${CUE_MAC_TARGETS:-aarch64-apple-darwin x86_64-apple-darwin}"

binaries=()
for target in "${targets[@]}"; do
    rustup target add "$target" > /dev/null
    cargo build -p cue-app --release --locked --target "$target"
    binaries+=("target/$target/release/cue")
done

app="$out/Cue.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/cue" "${binaries[@]}"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
cp assets/Cue.icns LICENSE "$app/Contents/Resources/"
if [[ -f THIRD_PARTY_LICENSES.html ]]; then
    cp THIRD_PARTY_LICENSES.html "$app/Contents/Resources/"
fi

# Not Developer ID signed or notarized yet. An ad-hoc signature is still required for
# the bundle to launch on Apple silicon.
codesign --force --deep --sign - "$app"

suffix="universal"
if [[ ${#targets[@]} -eq 1 ]]; then
    suffix="${targets[0]%%-*}"
fi
zip="Cue-$version-macos-$suffix.zip"
rm -f "$out/$zip"
(cd "$out" && ditto -c -k --keepParent Cue.app "$zip")
echo "$out/$zip"
