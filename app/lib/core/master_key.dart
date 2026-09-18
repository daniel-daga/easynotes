/// Where this device's master key lives.
///
/// One secret unlocks everything local: the note keys, the device identity and
/// the sync cursors are all sealed under it ([ADR 0005](../../../docs/adr/0005-local-store.md)).
/// It belongs in the OS keychain — Keystore, the Keychain, libsecret, DPAPI —
/// and the OS keychain is something Dart can reach and the Rust core cannot, so
/// this is the one piece of key handling that lives up here.
///
/// The core never chooses where the key is kept. It is handed one, and it fails
/// loudly if it is the wrong one, which is the behaviour you want from the layer
/// that cannot tell an empty device from a locked one.
library;

import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../src/rust/api/notes.dart' as rust;

/// How many bytes a master key is. `MasterKey::from_slice` rejects anything
/// else, which is the check that matters; this is here so the generator and the
/// core cannot drift quietly.
const masterKeyBytes = 32;

class MasterKeyStore {
  MasterKeyStore({FlutterSecureStorage? storage})
    : _storage =
          storage ??
          const FlutterSecureStorage(
            // `this_device`: the key is never restored onto another phone from
            // an iCloud backup. It would arrive there without the database it
            // unlocks, and a secret that travels further than it is any use is
            // a secret in more places than it needs to be. Android's defaults
            // already wrap the key with one held in Keystore.
            iOptions: IOSOptions(
              accessibility: KeychainAccessibility.first_unlock_this_device,
            ),
          );

  final FlutterSecureStorage _storage;

  /// The name under which the key is filed. One device, one key, so it does not
  /// need to say which notes it opens.
  static const _key = 'easynotes.master-key';

  /// The key for the notes stored in [directory], from the keychain — adopting
  /// the one beside the database if this device has it, and minting one if this
  /// is a device that has never held a note.
  ///
  /// The order matters. A device that updated into this version has a
  /// `master.key` file, and that file is the only thing its database opens
  /// with, so a fresh key would present the user with an empty app and their
  /// notes unreadable on disk beside it. The file is left where it is:
  /// deleting the only copy of the thing that unlocks everything, on the same
  /// launch that first writes it somewhere else, is how a migration turns a bug
  /// into a loss.
  Future<Uint8List> forDirectory(String directory) async {
    final stored = await _storage.read(key: _key);
    if (stored != null) return _decode(stored);

    final carriedOver = rust.masterKeyBesideTheDatabase(dir: directory);
    final key = carriedOver ?? newKey();
    await _storage.write(key: _key, value: base64Encode(key));
    return key;
  }

  /// The key for the notes a browser is holding, from the same keychain — and
  /// the honest account of what that is worth on the web.
  ///
  /// `flutter_secure_storage` on the web wraps the value with WebCrypto and
  /// keeps both the wrapped value and the wrapping key in this origin's
  /// `localStorage`. Anything that can run script on the origin can therefore
  /// read this key, and anything that can read this key can read the IndexedDB
  /// it unlocks — which is on the same origin. So the sealing buys nothing
  /// against an attacker who is already running here, and it is not claimed to.
  ///
  /// What it does buy is that the shape is the same everywhere: one secret, one
  /// place, handed to the core from outside it. The day a browser offers
  /// somewhere real to put it — a passphrase, or a WebAuthn PRF — it is this
  /// method that changes and nothing below it. [ADR
  /// 0011](../../../docs/adr/0011-the-web-store.md) has the rest.
  ///
  /// There is no file to carry over, because no version of this app ever wrote
  /// one in a browser.
  Future<Uint8List> forBrowser() async {
    final stored = await _storage.read(key: _key);
    if (stored != null) return _decode(stored);
    final key = newKey();
    await _storage.write(key: _key, value: base64Encode(key));
    return key;
  }

  /// A key for a device that has never had one.
  ///
  /// `Random.secure` is the platform's own CSPRNG on every target Flutter
  /// builds for. It is public because the tests need a key and have no keychain
  /// to keep one in.
  static Uint8List newKey() {
    final random = Random.secure();
    return Uint8List.fromList(
      List<int>.generate(masterKeyBytes, (_) => random.nextInt(256)),
    );
  }

  Uint8List _decode(String stored) {
    final key = base64Decode(stored);
    if (key.length != masterKeyBytes) {
      // Rather than hand the core something that cannot work and let it report
      // a database it cannot read.
      throw StateError(
        'the master key in the keychain is ${key.length} bytes, not '
        '$masterKeyBytes',
      );
    }
    return key;
  }
}
