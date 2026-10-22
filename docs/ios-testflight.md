# iOS TestFlight builds

`.github/workflows/ios-testflight.yml` builds a signed iOS IPA and uploads it to
the existing App Store Connect app (`eu.fraisl.easynotes`). It runs only when
manually dispatched. Uploading a build does not invite testers or submit it for
external beta review.

## One-time Apple setup

In Apple Developer, create an **Apple Distribution** certificate and export it
as a password-protected `.p12`. Create an **App Store** provisioning profile for
`eu.fraisl.easynotes`, using that certificate. The profile must be named
`AppStore eu.fraisl.easynotes`.

In App Store Connect, create an API key with **App Manager** access. Keep the
downloaded `.p8` file; Apple only makes the private key available once. Record
the key ID and issuer ID as well.

## GitHub configuration

In the repository's **Settings → Secrets and variables → Actions**, add these
repository variables:

| Variable | Value |
| --- | --- |
| `APPSTORE_ISSUER_ID` | App Store Connect issuer ID |
| `APPSTORE_API_KEY_ID` | App Store Connect API key ID |

Add these repository secrets:

| Secret | Value |
| --- | --- |
| `APPSTORE_API_PRIVATE_KEY` | Full contents of the downloaded `AuthKey_*.p8` file |
| `APPSTORE_CERTIFICATES_FILE_BASE64` | Base64 encoding of the `.p12` file, with no line breaks |
| `APPSTORE_CERTIFICATES_PASSWORD` | The `.p12` export password |

The workflow also reads the same published-detail variables the Android release
uses, and refuses to build without them:

| Variable | Value |
| --- | --- |
| `EASYNOTES_PRIVACY_URL` | The `https://` privacy policy, shown in the app's *About and privacy* screen |
| `EASYNOTES_SUPPORT` | The support, security and data-request contact, shown in the same screen |

They are baked into the app at build time as `--dart-define`s (see
`app/lib/ui/about_screen.dart`), so a store build shows the version, the policy
URL and the contact rather than "Development build".

For example, on macOS or Linux, encode the certificate with:

```sh
base64 -i distribution.p12 | tr -d '\n'
```

Do not commit the certificate, private API key, or their passwords to the
repository.

## Upload a build

Open **Actions → iOS TestFlight → Run workflow**, choose the branch or commit to
build, then dispatch it. The workflow uses the app's version from
`app/pubspec.yaml` and assigns a unique build number for every run and rerun.
Once Apple's processing completes, the build appears in the app's TestFlight
tab. Add internal testers there; external testing additionally requires the
TestFlight test information and Apple's beta review.
