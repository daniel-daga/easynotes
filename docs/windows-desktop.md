# Windows desktop

Verified on 2026-09-26: the release x64 build succeeds with Flutter 3.47.5
and Visual Studio 2022; all 40 first-launch and bridge tests pass against its
bundled Rust DLL, and `flutter analyze` reports no issues. The dependency lock
is unchanged. Native UI and OS integration checks below remain open.

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
The latter accepts a relay URL, like `just run-linux`. For a release build with
a remote relay, pass `--dart-define=EASYNOTES_RELAY=https://relay.example` to
`flutter build windows --release`. The default is `http://127.0.0.1:8080`.

The manually triggered **App builds** workflow has a Windows checkbox and
uploads the complete folder as `easynotes-windows-x64`. It also runs the
first-launch and Rust bridge tests against the DLL from that folder:

```powershell
cd app
$env:EASYNOTES_NATIVE_LIB = (Resolve-Path 'build/windows/x64/runner/Release/rust_lib_easynotes.dll').Path
flutter test test/first_launch_test.dart test/rust_bridge_test.dart
```

These tests exercise the packaged Rust library, persistence and bridge lifetime.
They mock secure storage and do not prove Windows key storage or notification
delivery. A native UI walkthrough, restart with the real key store, reminder
delivery, and two-device sync remain separate runtime checks. Windows invite
links currently use the drawer's **Join with a link**; native URL registration
and receiving text through the Windows share sheet are not implemented.
