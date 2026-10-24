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

## Where the build lands, and why testers may not see it

App Store Connect files an upload under two values from `app/pubspec.yaml`:
`CFBundleShortVersionString` is the version before the `+`, and
`CFBundleVersion` is the build number the workflow assigns. A run that builds
`0.1.5` therefore appears under a **0.1.5** version group, not under an older
one — check the version group, not just the top of the TestFlight tab.

An uploaded build is not distributed on its own. It stays invisible to testers
until one of these is true:

- the build is added to an internal tester group (**TestFlight → Internal
  Testing → the group → Builds → +**), or
- that group has automatic distribution enabled.

The workflow deliberately does neither, and it does not touch the TestFlight
test information or submit for beta review.

Two other states look like "the build never arrived":

- **Missing Compliance.** A build whose export-compliance question is
  unanswered is not distributable. `Info.plist` now declares
  `ITSAppUsesNonExemptEncryption = false`, so new uploads skip this; builds
  uploaded before that key existed need it answered by hand under the build's
  **Manage Compliance**.
- **Expiry.** Apple removes a build from TestFlight 90 days after upload.

If the workflow itself has not run since the version bump, nothing was uploaded
for that version. Note that the `App builds` workflow's `ios` job is a
simulator-only compile check with no certificate and no upload; it can never
produce a TestFlight build.
