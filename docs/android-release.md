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

Before tagging, write this version's compatibility record and commit it:
`just compat-fixtures`, or dispatch the **Compatibility fixtures** workflow and
unpack its artifact into `compat/`. The release workflow refuses a tag without
`compat/v<version>/`, and later builds read it back ([ADR
0029](adr/0029-compatibility-across-releases.md)).

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

## What has been checked, and what has not

Install, update over the previous release, the downgrade refusal, and a
reinstall with a restore from the recovery phrase were checked on a **Pixel 10
Pro running Android 16 on 2026-10-23**, with the signed `v0.1.5` draft over
`v0.1.4` (`versionCode` 1005 over 1004). The note and its text survived the
update, the certificate was unchanged, and the downgrade was refused.

The same install, update and downgrade sequence, from `0.1.0` through `0.1.2`,
had run on the `Medium_Phone` x86_64 emulator (Android 16 / API 36) on
2026-10-22.

A two-device sharing run on **2026-10-26** used a Pixel 10 Pro and an iPad.
Share, join, the shared colour, attachment add/remove and revoke passed. Live
collaboration still needs polish, offline edits were not tried, and deleting
the guest's account left it in the owner's People list. The build, OS versions
and relay operator were not recorded, and an iPad is not a second Android
phone, so this is **not** gate item 2.
[`two-phone-run-2026-10-26.md`](two-phone-run-2026-10-26.md) records it, and
[#63](https://github.com/daniel-daga/easynotes/issues/63) tracks the two-Android
rerun.

This is not the whole app working. The sharing journey — share, join, offline
edits, revoke, attachments and account deletion — still has to pass on two
physical Android phones against a TLS relay, and no draft has been published or
uploaded to Play. That is launch gate item 2; publishing and the Play upload
are also still open. See the [production readiness
report](production-readiness-2026-09-30.md#launch-gate-updated).
