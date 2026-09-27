# Building an Android release

A release build is signed with the release key or not signed at all. It is
never debug-signed any more: a debug-signed APK cannot be updated by a
store-signed one, and it had been what `flutter build apk --release` produced.

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

The two URLs are what *About and privacy* shows (`lib/ui/about_screen.dart`).
A build without them says it was not published with a policy, which is right
for a test build and wrong for a store.

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
flutter build apk --release --build-name 1.2.3 --build-number 1002003 \
    --dart-define=EASYNOTES_VERSION=1.2.3 …
```

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

## The release workflow

Push a tag `vMAJOR.MINOR.PATCH` and `.github/workflows/release.yml` runs the CI
lint and tests, builds the bundle and an APK for arm64, armv7 and x64 with a
pinned Flutter and Rust, and then refuses the result unless the APK:

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

## What this does not prove

That the app works. A release build still has to be installed on real phones
and taken through the flows in the [production readiness
review](production-readiness-2026-09-27.md) — install, update over the previous
release, share, revoke, reinstall and restore — before it is anything but a
test build.
