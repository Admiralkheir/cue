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

# macOS ties Screen Recording to the app's designated requirement. An ad-hoc signature's
# requirement is its cdhash, which every build changes: after a rebuild or an update,
# System Settings still shows Cue as allowed but the grant no longer applies, and macOS
# doesn't ask again. Signing with a certificate makes the requirement "this bundle id,
# signed by this certificate", which stays the same from build to build. See
# packaging/macos/SIGNING.md.
#
# CUE_MACOS_SIGN_IDENTITY picks the identity (its name or SHA-1). Without it, a
# "Cue Local Signing" certificate made by scripts/macos-signing-cert.sh is used when the
# keychain has one, and otherwise the bundle is signed ad hoc, which Apple silicon still
# needs to launch it.
identity="${CUE_MACOS_SIGN_IDENTITY:-}"
if [[ -z "$identity" ]] && security find-identity -p codesigning | grep -q '"Cue Local Signing"'; then
    identity="Cue Local Signing"
fi
if [[ -z "$identity" ]]; then
    echo "warning: signing ad hoc; Screen Recording has to be allowed again after every" \
        "build (see packaging/macos/SIGNING.md)" >&2
    identity="-"
fi
codesign --force --sign "$identity" "$app"
codesign --verify --strict "$app"
codesign --display --requirements - "$app" 2>&1 | grep '^designated'

suffix="universal"
if [[ ${#targets[@]} -eq 1 ]]; then
    suffix="${targets[0]%%-*}"
fi
zip="Cue-$version-macos-$suffix.zip"
rm -f "$out/$zip"
(cd "$out" && ditto -c -k --keepParent Cue.app "$zip")
echo "$out/$zip"
