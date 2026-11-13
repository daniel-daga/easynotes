# Building an Android release

A release build is signed with a configured release key or left unsigned. It
is not debug-signed. See the [project README](../README.md#getting-started) for
the Rust and Flutter setup before following this guide.

## The signing key

Make one upload key, once, and keep it and its passwords backed up outside the
repository and outside GitHub. Every installed copy of the app trusts it: an
update signed with a different key cannot replace one signed with this, so
losing it means publishing a new app under a new ID.

```sh
keytool -genkeypair -v -keystore upload.jks -storetype JKS \
    -keyalg RSA -keysize 4096 -validity 10000 -alias upload
```

For Google Play, enrol in Play App Signing: Play keeps the key that signs what
users install, and this one only proves an upload came from you, so it can be
reset through Play support if it is lost.

F-Droid signs its own builds with its own key (see [F-Droid](#f-droid) below).

## A local signed build

Create `app/android/key.properties` — it is in `.gitignore`, and must stay
there:

```properties
storeFile=/absolute/path/to/upload.jks
storePassword=…
keyAlias=upload
keyPassword=…
```

A relative `storeFile` is resolved from `app/android/app`. The same four values
can come from `EASYNOTES_KEYSTORE`, `EASYNOTES_KEYSTORE_PASSWORD`,
`EASYNOTES_KEY_ALIAS` and `EASYNOTES_KEY_PASSWORD` instead, which is how the
release workflow passes them. Without either, a release build is unsigned —
the form F-Droid builds and then signs itself.

```sh
cd app
flutter build appbundle --release \
    --build-name 1.2.3 --build-number 1002003 \
    --dart-define=EASYNOTES_VERSION=1.2.3 \
    --dart-define=EASYNOTES_PRIVACY_URL=https://example.org/privacy \
    --dart-define=EASYNOTES_SUPPORT=support@example.org
```

The privacy URL and support contact are shown in *About and privacy*
(`app/lib/ui/about_screen.dart`). Replace the examples with your own published
details. A build without them is a development build, not a store-ready release.
Forks should also set `EASYNOTES_SOURCE_URL` to their corresponding source URL.

## Billing for a hosted relay

A hosted relay mints a per-device access token ([ADR
0038](decisions.md#adr-0038)) rather than handing out one shared key. Point a
build at the billing service with:

```sh
--dart-define=EASYNOTES_BILLING=https://billing.example.org
```

Without the define `billingBase` is empty, no request is made, and the app uses
the profile's own access key — the self-hosted behaviour. The address must be
`https://`; plain `http://` is accepted only on loopback for development. The
app's whole contract with the service is:

- `POST /v1/guest-token`, for someone a note is shared with;
- `POST /v1/device-token`, for a device being added to the account;
- both with `Authorization: Bearer <device token>`, and both answering
  `{"access":"<id>.<secret>"}`.

The app sends no note, account or recovery phrase, and does not follow a
redirect with that bearer token. The release and TestFlight workflows pass
`EASYNOTES_BILLING` from the repository variable of the same name, so the
hosted build sets it and a build without it keeps the self-hosted behaviour.

## Exact alarms and the Play declaration

The app asks for `SCHEDULE_EXACT_ALARM` so a reminder set for 09:00 fires at
09:00. Android 14 and later do not grant it on install. "Add reminder" opens
the system page that does, and until it is granted, or if it is withdrawn,
reminders are scheduled inexactly and may fire some minutes late while the
phone is idle. Coming back to the app reschedules every reminder in whichever
mode is now allowed.

Google Play asks every app that declares this permission to justify it in the
Play Console (**App content → Sensitive app permissions → Exact alarms**). The
case to make: time-based reminders on notes are a core, user-initiated
function, each one set by the person for a time they chose, like a calendar
entry; nothing is scheduled that the person did not ask for. Play may still
decide that a notes app does not qualify. If it does, remove the permission
from `AndroidManifest.xml` and the release check will want nothing else: the
scheduler already falls back to inexact reminders on its own.

## F-Droid

F-Droid builds the tagged source itself and signs with its own key, under the
same application ID. So a copy installed from F-Droid and one from Play cannot
update each other: switching stores means uninstalling, which on a
local-only profile deletes its notes. Both listings, and the website, have to
say so. The build F-Droid needs is the unsigned release this Gradle file makes
when no key is configured:

```sh
cd app/android
./gradlew assembleRelease
```

Back at the repository root, the APK is
`app/build/app/outputs/apk/release/app-release-unsigned.apk`. Flutter's own
builder wants a signed `app-release.apk` and fails on this one, which is why the
command is Gradle's `assembleRelease` and not `flutter build apk --release`.

## Store text

`fastlane/metadata/android/en-US/` holds the listing F-Droid reads — title,
summary, description, icon — and one changelog per release, named by its
versionCode (`changelogs/1002003.txt` for 1.2.3). The release workflow uses the
same file for the GitHub release notes and refuses a tag that has none, so a
release is described once. Play's listing is entered in the Play Console; copy
it from here so the two stores say the same thing.

Google Play also needs an account-deletion URL.
[`docs/public/account-deletion.md`](public/account-deletion.md) is written to be
that page: publish it beside the privacy policy.

## Versions

`versionCode` is `MAJOR*1000000 + MINOR*1000 + PATCH`, from the tag, so that it
only goes up while the version does and a rebuild of the same tag — F-Droid's,
for instance — arrives at the same number. `pubspec.yaml`'s version is what a
local build without `--build-name` says.

Before tagging, write this version's compatibility record and commit it:
`just compat-fixtures`, or dispatch the **Compatibility fixtures** workflow and
unpack its artifact into `compat/`. The release workflow refuses a tag without
`compat/v<version>/`, and later builds read it back
([ADR 0029](decisions.md#adr-0029)).

## The release workflow

Dispatch `.github/workflows/release.yml` for a tag `vMAJOR.MINOR.PATCH` — from
Actions, or with `gh workflow run release.yml -f release_tag=v0.1.2`. Pushing
the tag does **not** start it: the workflow is `workflow_dispatch` only. It
checks out the tag, runs the CI lint and tests, builds the bundle and an APK for
arm64, armv7 and x64 with a pinned Flutter and Rust, and then refuses the result
unless the APK:

- is `eu.fraisl.easynotes` with the tag's `versionCode`, targeting API 36 or
  later, and not debuggable;
- does not request `USE_EXACT_ALARM`;
- disallows backup (`res/xml/data_extraction_rules.xml` says why);
- carries the core for all three ABIs;
- is signed with the certificate whose SHA-256 is in `EASYNOTES_CERT_SHA256`.

It then writes `SHA256SUMS`, a Rust dependency and licence list, the Dart lock
file and a build provenance attestation, and leaves a **draft** GitHub release.
Publishing it, and uploading the bundle to Play's internal track, is a
person's decision. The top of the workflow lists the secrets and variables it
needs; it stops at its first step if any is missing.

## Validate before publishing

A successful build is not a validated release. Test the signed build on
physical Android devices against a TLS relay, using disposable test accounts:

- Install, update from the previous version, verify signature continuity and
  check that a downgrade is refused without losing notes.
- Share and join a note; edit concurrently and offline; reconnect and check
  convergence, including after a force-stop.
- Add/remove attachments, revoke a collaborator, and verify that future edits
  are not available to the removed account.
- Reinstall and recover on the same relay, then delete the account and verify
  the effects on owned notes, shared notes and other devices.
- Check reminder permissions and delivery, and the published privacy/support
  links. The pages in `docs/public/` are drafts, not ready-to-publish policies.

Record the build, devices, OS versions and relay configuration with the test
results. The [offline test tool](../tools/android-gate/README.md) covers part
of this journey; emulator checks do not replace hardware validation. See
[project status](../README.md#project-status) for current support limits.
