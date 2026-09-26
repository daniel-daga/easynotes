# Windows desktop

Verified on 2026-09-26: the release x64 build succeeds with Flutter 3.47.5
and Visual Studio 2022; all 40 first-launch and bridge tests pass against its
bundled Rust DLL, and `flutter analyze` reports no issues. A separate-process
smoke check also saved a note with a temporary key in Windows secure storage,
reopened it from a second process, and removed its key. The dependency lock is
unchanged. Native UI, reminders and two-device checks remain open.

The Windows x64 runner is in `app/windows`. It bundles the Rust core through
the existing Cargokit plugin, plus Flutter's secure-storage, timezone and local
notification plugins. Build with Flutter and Visual Studio's **Desktop
development with C++** workload (including CMake and the Windows SDK).

On the project's Windows machine, make Rust available to Cargokit first:

```powershell
$env:PATH = "$env:USERPROFILE\scoop\persist\rustup\.cargo\bin;" + $env:PATH
cd app
flutter pub get
flutter build windows --release
```

The app is `app/build/windows/x64/runner/Release/easynotes.exe`. Distribute the
whole `Release` folder: the executable needs the DLLs and `data` beside it.
The folder is an unpackaged build, without an installer or code signing.

`just build-windows` builds it; `just run-windows` launches a development build.
Choose the relay in **Relay settings** after the app starts. This setting is
saved on the device and works in release builds too.

`just windows-storage-smoke` runs those three steps locally and leaves the
normal release build in place. The manually triggered **App builds** workflow has a Windows checkbox and
uploads the complete folder as `easynotes-windows-x64`. It also runs the
first-launch and Rust bridge tests against the DLL from that folder:

```powershell
cd app
$env:EASYNOTES_NATIVE_LIB = (Resolve-Path 'build/windows/x64/runner/Release/rust_lib_easynotes.dll').Path
flutter test test/first_launch_test.dart test/rust_bridge_test.dart
```

These tests exercise the packaged Rust library, persistence and bridge lifetime.
They mock secure storage. To exercise the Windows key store itself, the manual
workflow builds a small alternate entrypoint and runs:

```powershell
flutter build windows --release --target tool/windows_storage_smoke.dart
../tools/windows-storage-smoke.ps1
flutter build windows --release
```

It uses a fresh temporary SQLite directory and a separate DPAPI-backed test
key. Two short-lived processes write and reopen the note; the script then
deletes only that test key and the temporary directory. It never touches the
production key. A native UI walkthrough, reminder delivery, and two-device sync
remain separate runtime checks. Windows invite links currently use the drawer's
**Join with a link**; native URL registration and receiving text through the
Windows share sheet are not implemented.
