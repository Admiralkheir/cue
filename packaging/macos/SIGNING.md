# Signing Cue.app

## Why it matters: Screen Recording

macOS remembers a Screen Recording grant together with the app's *designated
requirement*, the rule that says which code counts as "this app". What that rule is
depends on how the app is signed:

| Signature | Designated requirement | After a rebuild or update |
| --- | --- | --- |
| Ad hoc (`codesign -s -`) | `cdhash H"…"`, the hash of this exact build | grant no longer applies |
| Self-signed certificate | `identifier "io.github.Admiralkheir.Cue" and certificate leaf = H"…"` | grant kept |
| Developer ID | `identifier … and anchor apple generic and … leaf[subject.OU] = TEAMID` | grant kept |

With an ad-hoc signature, every build is a new app as far as TCC is concerned. The old
grant stays in System Settings and still shows as **on**, but it no longer matches, so
`CGPreflightScreenCaptureAccess` returns false. `CGRequestScreenCaptureAccess` shows no
prompt either, since an entry already exists, and switching it off and on again doesn't
help. The only way out is to remove Cue from the list with **−** and then add it back
(with **+**, or by opening Cue and allowing it when asked).

The certificate doesn't stop the keychain from asking: a Cue.app rebuilt and signed with
the same certificate still asked for the login password before reading a stored API key.

This was checked on macOS 26.7 (Apple silicon) with small probe apps signed exactly like
Cue.app. Ad-hoc signing by itself is not the problem: a universal binary signed with
`--deep` gets and keeps the permission like any other app, as long as it's never rebuilt.
After a rebuild it lost access. The same rebuild signed with a self-signed certificate
kept it, and so did the real Cue.app. ScreenCaptureKit (`SCShareableContent`) followed
`CGPreflightScreenCaptureAccess` in every case, so switching capture APIs wouldn't change
this. An ad-hoc signature with an explicit `designated => identifier "…"` requirement
also kept the grant, but then any app that claims Cue's bundle id would get Cue's
permission, so it isn't used.

## Local builds

Make a certificate once:

```sh
scripts/macos-signing-cert.sh
```

This adds a self-signed "Cue Local Signing" identity to the login keychain.
`scripts/package-macos.sh` signs with it whenever it's there; otherwise it signs ad hoc
and warns. `CUE_MACOS_SIGN_IDENTITY` picks another identity. Allow Screen Recording for
`Cue.app` once; later builds of it keep the permission.

`cargo run -p cue-app` isn't affected: it records the screen with the terminal's
permission.

## Releases

`.github/workflows/package.yml` imports the certificate from two repository secrets
before packaging (`scripts/ci-import-macos-cert.sh`):

- `MACOS_SIGN_P12`: the certificate and private key as a base64 PKCS#12 file
- `MACOS_SIGN_PASSWORD`: its password

Without them, the build is signed ad hoc and every update makes users allow Screen
Recording again. To set them up:

```sh
scripts/macos-signing-cert.sh "Cue Release Signing" ~/cue-release-signing.p12
base64 -i ~/cue-release-signing.p12 | gh secret set MACOS_SIGN_P12
gh secret set MACOS_SIGN_PASSWORD
```

Keep the `.p12` file and its password somewhere safe, such as a password manager.
Switching to a different certificate later makes every user allow Screen Recording once
more. The same happens once when users move from an ad-hoc release to the first signed
one.

A self-signed certificate doesn't help with Gatekeeper. A downloaded Cue.app is rejected
the same way as an ad-hoc one, so users still click "Open Anyway" the first time.

## Developer ID (later)

Fixing Gatekeeper too takes an Apple Developer Program membership (99 USD a year):

1. Create a *Developer ID Application* certificate and store it in the same two secrets.
   The import script and `package-macos.sh` work with it unchanged.
2. Sign with the hardened runtime and a timestamp: add `--options runtime --timestamp` to
   the `codesign` call.
3. Notarize and staple the zip in CI with `xcrun notarytool submit --wait`, using an App
   Store Connect API key stored as more secrets, then run `xcrun stapler staple Cue.app`
   and zip it again.

Moving from the self-signed certificate to Developer ID changes the designated
requirement once, so users allow Screen Recording one more time.
