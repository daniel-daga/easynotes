# iOS builds and releases

iOS has build and TestFlight upload support, but device validation is incomplete.
See [project status](../README.md#project-status). Building or uploading an IPA
does not make it ready for distribution.

## Local builds

Use macOS with Xcode, its iOS SDK and command-line tools, and the Rust/Flutter
setup in the [README](../README.md#getting-started). Run `flutter doctor` to
check platform prerequisites, then from the repository root:

```sh
rustup target add aarch64-apple-ios
cd app
flutter pub get
flutter build ios --release --no-codesign
```

This builds without signing; it does not produce an installable App Store IPA.
For a device development run, open `app/ios/Runner.xcworkspace` in Xcode,
configure your signing team, and use `flutter run` from `app/` with a connected
device. Forks need their own bundle ID, signing team and provisioning profile.

## TestFlight workflow

The manually dispatched [iOS TestFlight workflow](../.github/workflows/ios-testflight.yml)
builds a signed IPA, uploads it to App Store Connect and waits for processing.
It does not add testers or submit for external beta or App Store review.
The workflow pins the toolchains, bundle ID and Apple team; forks must change
the app and workflow configuration to match their own Apple account.

### Apple setup

1. Create an **Apple Distribution** certificate and export it as a
   password-protected `.p12`.
2. Create an **App Store** provisioning profile for the configured bundle ID,
   using that certificate. The workflow expects the profile name
   `AppStore <bundle-id>`.
3. Create an App Store Connect API key with **App Manager** access. Save the
   `.p8` private key securely; Apple makes it available only once. Record the
   key ID and issuer ID.

### GitHub configuration

Under **Settings → Secrets and variables → Actions**, configure:

| Repository variable | Purpose |
| --- | --- |
| `APPSTORE_ISSUER_ID` | App Store Connect issuer ID |
| `APPSTORE_API_KEY_ID` | App Store Connect API key ID |
| `EASYNOTES_PRIVACY_URL` | Published HTTPS privacy policy URL |
| `EASYNOTES_SUPPORT` | Support and data-request contact shown in the app |
| `EASYNOTES_BILLING` | Optional external token-service URL; leave unset for self-hosted builds |

| Repository secret | Purpose |
| --- | --- |
| `APPSTORE_API_PRIVATE_KEY` | Full contents of `AuthKey_*.p8` |
| `APPSTORE_CERTIFICATES_FILE_BASE64` | Base64-encoded `.p12`, without line breaks |
| `APPSTORE_CERTIFICATES_PASSWORD` | Certificate export password |

On macOS, encode the certificate with:

```sh
base64 -i distribution.p12 | tr -d '\n'
```

Do not commit certificates, private keys or passwords. The workflow embeds the
version, privacy URL, support contact and optional token-service URL in the
app. The service contract is described in the
[Android build guide](android-release.md#billing-for-a-hosted-relay).

### Upload and distribute

Open **Actions → iOS TestFlight → Run workflow** and select the branch or commit.
The app version comes from `app/pubspec.yaml`; the workflow assigns a unique
build number per run and rerun.

After processing, find the matching version group in App Store Connect's
**TestFlight** tab. Add the build to an internal tester group, or enable that
group's automatic distribution. External testing additionally requires test
information, a contact and Apple's beta review. TestFlight builds expire after
90 days.

The **App builds** workflow's iOS simulator check does not sign or upload a
TestFlight build.

## Before App Store submission

### Store information and privacy

- Complete the support URL, category, age rating, screenshots and review
  information in App Store Connect. A marketing URL is optional.
- Store metadata is in `fastlane/metadata/ios/en-US/`; check that it matches
  the app's display name and current behaviour.
- Review camera and microphone purpose strings in `app/ios/Runner/Info.plist`
  against the features that use those permissions.
- Complete **App Privacy** using the actual app, relay and any external
  services in your build. See [SECURITY.md](../SECURITY.md) for relay metadata.
- Publish a privacy policy appropriate to your deployment. The files in
  `docs/public/` are drafts with operator-specific information still required.

### Export compliance

`app/ios/Runner/Info.plist` currently declares
`ITSAppUsesNonExemptEncryption = false`. This is a configuration value, not a
legal determination that every build or distribution qualifies for an exemption.
The publisher must verify Apple's current export-compliance requirements and
applicable law for the cryptography they distribute, and update the declaration
or provide documentation when required.

An older build with unanswered compliance questions may remain in **Missing
Compliance**. Check the build's **Manage Compliance** page in App Store Connect.

### Device validation

Test the release build on physical iPhones against a TLS relay with disposable
accounts. Cover share/join, concurrent and offline edits, revocation, reminder
delivery, attachment add/remove and interrupted uploads, reinstall and recovery,
and account deletion. Check deep links and the share sheet separately from
simulator compilation. Record the build, devices, OS versions and relay setup.

On iOS, recovery phrases and transfer QRs have no screenshot or Recents block;
see [known security limitations](../SECURITY.md#scope).

Only submit for review after the build, device checks, store information,
privacy policy and support contacts have been verified.
