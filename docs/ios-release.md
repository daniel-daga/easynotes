# iOS release checklist

**Date:** 2026-10-22 · App: `eu.fraisl.easynotes` · Team `86XL7XR5XV`

The iOS build itself is in place and working: `.github/workflows/ios-testflight.yml`
builds a signed IPA from the built-in Flutter iOS project, uploads it to App
Store Connect, and waits for Apple's processing (last green run 2026-10-18).
This page records what stands between that build and shipping it, and where each
thing happens. It is the iOS counterpart of
[`android-release.md`](android-release.md); the shared privacy, security and
operational gate items are not repeated here.

## Done

- Signed TestFlight build uploads, with a per-run build number.
- The About screen is wired to `EASYNOTES_VERSION`, `EASYNOTES_PRIVACY_URL` and
  `EASYNOTES_SUPPORT`; the workflow now bakes all three in (was missing, so
  early builds said "Development build" with no policy or contact).
- In-app camera and microphone purpose strings, the `easynotes://` URL scheme,
  deep-link handling, and the iOS share sheet (see `app/ios/Runner/`).
- App icons at every size in `app/ios/Runner/Assets.xcassets`.
- Store copy drafted under `fastlane/metadata/ios/en-US/` (name, subtitle,
  keywords, description, privacy URL, release notes).

## Still to do, in order

### 1. Fill in the remaining App Store Connect fields

`fastlane/metadata/ios/en-US/` has the copy, but two fields have no value in
this repository and must be supplied by the operator:

| Field | Where | Notes |
| --- | --- | --- |
| Support URL | App Store Connect → App Information | Must be an `http(s)://` URL, unlike the in-app contact (which may be an email). A page on `easynotes.fraisl.eu` is the natural home. |
| Marketing URL | App Store Connect → App Information | Optional. |
| App name (display) | App Store Connect | `Info.plist` says `CFBundleDisplayName = "Easynotes"` while the Android title is "EasyNotes". Pick one; the store name should match what users see. |
| Category (primary/secondary) | App Store Connect | Suggest Productivity. |
| Age rating | App Store Connect | Answer Apple's questionnaire (the app has no account sign-up and no user-generated public content, which keeps the rating low). |
| Screenshots | App Store Connect | Required for review; take them from a release build. |

The drafted copy is plain text because the App Store description does not
render HTML, unlike Google Play's.

### 2. Answer the export-compliance question

The app is end-to-end encrypted, and the encryption it uses falls under Apple's
exemption: standard TLS on the wire plus its own end-to-end note encryption,
with nothing that Apple considers non-exempt. `Info.plist` therefore declares

```xml
<key>ITSAppUsesNonExemptEncryption</key>
<false/>
```

so App Store Connect no longer asks the "Export Compliance" question for each
upload. Without the key every build sat in **Missing Compliance** and was never
distributed to testers, even though the upload itself succeeded.

This is a legal/export determination rather than a code change. If the
cryptography ever stops qualifying for the exemption — a new non-exempt
algorithm, or shipping third-party crypto outside TLS — flip the answer and file
the declaration instead of leaving the question unanswered at submission.

Builds uploaded before the key existed still need their compliance answered by
hand: App Store Connect → TestFlight → the build → **Manage Compliance**.

### 3. Complete App Privacy (App Store Connect)

App Store Connect's **App Privacy** section needs the data-collection answers,
mirroring Play's Data safety. The app has no analytics, advertising or crash
reporting, but the relay sees IP addresses, a random device key, note IDs,
timestamps and sizes (see the About screen and
[ADR 0014](decisions.md#adr-0014)). Fill it from that list, and make
sure it matches the published privacy policy.

### 4. External beta (TestFlight)

The workflow uploads the build but does not add testers or start a beta review.
To distribute externally:

- Add internal testers in App Store Connect.
- For external testing, fill the TestFlight **test information** (what to test,
  a contact email, review notes) and submit the build for Apple's beta review.

### 5. Run the hardware walkthrough on two iPhones

The Android side is still owed the same thing. On **2026-10-26** one iPad
completed share, join, the shared colour, attachment add/remove and revoke
against a TLS relay, but live collaboration still needs polish, offline edits
were not tried, and deleting the guest's account left it in the owner's People
list. That is a useful partial data point, not the walkthrough. Repeat the
release gate on two physical iPhones against a TLS relay:

share → join → concurrent and offline edits → revoke → attachment add and
remove (camera, gallery, large file on mobile data, background kill mid-upload)
→ reinstall → restore from recovery phrase → delete account. Record device, OS
and build. The camera string in `Info.plist` currently mentions only the QR
transfer, so confirm the attachment flows do not hit the camera without a
matching purpose string (attachments use the document picker, not the camera).
The 2026-10-26 Pixel + iPad run was a partial data point, not this walkthrough.

### 6. Publish

When everything above is recorded, submit the build for App Store review. iOS
being built does not by itself make the product ready: the privacy policy,
operator identity, support and security contacts, and the restore drill apply
to every store.
