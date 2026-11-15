#!/usr/bin/env bash
# Makes a self-signed code-signing certificate for Cue and adds it to the login keychain,
# so that Cue.app keeps its Screen Recording permission from one build to the next.
#
#   scripts/macos-signing-cert.sh [name] [export.p12]
#
# The name defaults to "Cue Local Signing", which scripts/package-macos.sh picks up on its
# own. With an export path, the certificate and its private key are also written there,
# behind a generated password, for storing as the MACOS_SIGN_P12 and MACOS_SIGN_PASSWORD
# repository secrets that release builds sign with. Keep that file somewhere safe: a new
# certificate means every user has to allow Screen Recording once more.
set -euo pipefail

name="${1:-Cue Local Signing}"
export_to="${2:-}"

if security find-identity -p codesigning | grep -q "\"$name\""; then
    echo "The keychain already has a code-signing identity named \"$name\"." >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat > "$work/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

# The system's LibreSSL writes a PKCS#12 file the keychain can import; OpenSSL 3 would
# need -legacy for that.
/usr/bin/openssl req -x509 -newkey rsa:3072 -nodes -days 3650 -config "$work/cert.cnf" \
    -keyout "$work/key.pem" -out "$work/cert.pem" 2> /dev/null
password="$(/usr/bin/openssl rand -base64 24)"
/usr/bin/openssl pkcs12 -export -name "$name" -inkey "$work/key.pem" -in "$work/cert.pem" \
    -out "$work/cert.p12" -passout "pass:$password"

security import "$work/cert.p12" -k "$HOME/Library/Keychains/login.keychain-db" \
    -P "$password" -T /usr/bin/codesign
echo "Added \"$name\" to the login keychain."

if [[ -n "$export_to" ]]; then
    cp "$work/cert.p12" "$export_to"
    chmod 600 "$export_to"
    echo
    echo "Wrote $export_to. Its password: $password"
    echo "To let release builds sign with it:"
    echo "  base64 -i '$export_to' | gh secret set MACOS_SIGN_P12"
    echo "  gh secret set MACOS_SIGN_PASSWORD   # paste the password above"
fi
