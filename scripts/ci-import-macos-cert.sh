#!/usr/bin/env bash
# CI only: imports the certificate in MACOS_SIGN_P12 (base64) and MACOS_SIGN_PASSWORD into
# a throwaway keychain and hands its identity to scripts/package-macos.sh through
# GITHUB_ENV. Without the secrets (forks, or before they're set up) the build is signed
# ad hoc, and Screen Recording has to be allowed again after every update.
set -euo pipefail

if [[ -z "${MACOS_SIGN_P12:-}" ]]; then
    echo "::warning::MACOS_SIGN_P12 isn't set; Cue.app will be signed ad hoc"
    exit 0
fi

keychain="$RUNNER_TEMP/cue-signing.keychain-db"
keychain_password="$(openssl rand -base64 24)"
p12="$RUNNER_TEMP/cue-signing.p12"
trap 'rm -f "$p12"' EXIT

echo "$MACOS_SIGN_P12" | base64 --decode > "$p12"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$p12" -k "$keychain" -P "${MACOS_SIGN_PASSWORD:-}" -T /usr/bin/codesign
# Lets codesign use the key without a keychain prompt, which nobody could answer here.
security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" \
    > /dev/null
# shellcheck disable=SC2046 # the existing list is several quoted paths
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')

identity="$(security find-identity -p codesigning "$keychain" | awk '/^ *1\)/ { print $2; exit }')"
if [[ -z "$identity" ]]; then
    echo "::error::MACOS_SIGN_P12 holds no code-signing identity"
    exit 1
fi
echo "CUE_MACOS_SIGN_IDENTITY=$identity" >> "$GITHUB_ENV"
echo "Signing with $identity"
