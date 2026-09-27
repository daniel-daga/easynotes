/// Opening one relay profile's notes on a device with a disk.
library;

import 'master_key.dart';
import 'relay_profiles.dart';
import 'rust_note_core.dart';
import 'rust_note_library.dart';

/// Open [profile]'s notes under [baseDirectory], and start syncing them with
/// its relay when [connect] is true.
///
/// Each profile has a directory and a keychain entry of its own, so nothing
/// one relay's profile holds — account, device identity, note keys, cursors,
/// queued edits — is in reach of another's. The app passes its application
/// support directory; a test passes a temporary one.
Future<RustNoteLibrary> openProfileLibrary(
  String baseDirectory,
  RelayProfile profile, {
  bool connect = true,
  MasterKeyStore? keys,
}) async {
  // The bridge up front. Not load-bearing any more — every entry point below
  // brings it up itself, including [MasterKeyStore.forDirectory], which is the
  // one that crashed a real device's first launch by crossing the bridge before
  // anything had opened it. Kept because this is where the app would rather
  // find out that the native library is missing than three calls later.
  await RustNoteCore.ensureInitialized();
  // The keychain first, because it is what says whether this device has a key
  // at all — and because a device that updated into this version has one in a
  // file that has to be adopted rather than replaced.
  final directory = '$baseDirectory/easynotes-${profile.id}';
  final masterKey = await (keys ?? MasterKeyStore()).forDirectory(
    directory,
    profileId: profile.id,
  );
  final library = await RustNoteLibrary.open(directory, masterKey);
  // Connecting cannot fail loudly enough to matter: with no relay the app is a
  // local notes app, which is what it is designed to be when the network is
  // gone.
  if (connect && profile.url != null) await library.connect(profile.url!);
  return library;
}
