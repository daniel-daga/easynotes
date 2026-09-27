/// The one library the app is using, and moving it to another relay.
///
/// Each relay has a profile of its own — its own store, keys, account and
/// sync state — so moving to another relay is not reconnecting a library: it is
/// closing this profile's library and opening that one's
/// ([`docs/plan-relay-settings.md`](../../../docs/plan-relay-settings.md)).
/// This is the order that has to happen in, kept out of the widget tree so a
/// test can hold it to it without a device.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import 'note_library.dart';
import 'relay_profiles.dart';

/// Opens [profile]'s library, connected to its relay when [connect] is true.
///
/// [allowEphemeral] is whether a browser that refuses the app a database may
/// have its notes in memory instead. A switch never allows it: an empty tab in
/// place of a profile that has notes elsewhere would look like the notes had
/// gone.
typedef LibraryOpener =
    Future<NoteLibrary> Function(
      RelayProfile profile, {
      bool connect,
      bool allowEphemeral,
    });

class RelaySession extends ChangeNotifier {
  RelaySession({
    required this.profiles,
    required NoteLibrary library,
    required RelayProfile profile,
    required LibraryOpener open,
  }) : _library = library,
       _profile = profile,
       _open = open;

  final RelayProfiles profiles;
  final LibraryOpener _open;
  NoteLibrary _library;
  RelayProfile _profile;
  Future<void>? _switching;

  /// What the app stops using a library for before it is closed: reminders,
  /// alerts, lifecycle callbacks. Run on the way out of every library this
  /// session replaces, before the library is disposed.
  Future<void> Function(NoteLibrary library)? onRelease;

  /// The other half of [onRelease], run on the library that replaced it.
  void Function(NoteLibrary library)? onInstall;

  NoteLibrary get library => _library;
  RelayProfile get profile => _profile;

  /// How many times this profile's library has been opened again in place,
  /// so that whatever shows it can tell a fresh library from the one before.
  int get generation => _generation;
  int _generation = 0;
  bool get switching => _switching != null;

  /// Close this profile's library and open the one for [address] — a relay
  /// URL, or null for the profile that stays on this device.
  ///
  /// The same relay again, however it is spelled, is no switch at all, and a
  /// second call while one is under way waits for that one rather than
  /// starting another. Refused, with this profile still open and untouched,
  /// if its notes cannot be kept: a tab with no storage, or a store that has
  /// refused a write. Once this profile's library is closed, a failure
  /// reopens it, so a switch either completes or leaves the app where it was.
  Future<void> switchTo(String? address) {
    final running = _switching;
    if (running != null) return running;
    final url = address == null ? null : canonicalRelayUrl(address);
    if (url == _profile.url) return Future.value();
    final done = _switch(url).whenComplete(() {
      _switching = null;
      notifyListeners();
    });
    _switching = done;
    notifyListeners();
    return done;
  }

  Future<void> _switch(String? url) async {
    final old = _library;
    final previous = _profile;
    if (!old.canJoin) {
      throw StateError(
        'This tab has no storage, so its notes would be lost. Keep it open, '
        'and switch relay in a tab that has storage.',
      );
    }
    await old.flush();
    if (old.storageError != null) {
      throw StateError(
        'Some changes here could not be saved. Switching would lose them.',
      );
    }
    final target = await profiles.prepare(url);
    // Opened before anything is closed: a store that cannot be opened is
    // found out while the old one is still there to go back to.
    final next = await _open(target, connect: false, allowEphemeral: false);
    var closed = false;
    try {
      await onRelease?.call(old);
      // From here the old library may be half shut, so a failure reopens it
      // rather than carrying on with it.
      closed = true;
      await old.dispose();
      // Dispose writes out whatever was still queued. A refusal then means
      // an edit made during the switch was lost, and this profile is where
      // the person has to see that, not the next one.
      if (old.storageError != null) {
        throw StateError(
          'Some changes here could not be saved. Switching would lose them.',
        );
      }
      await profiles.activate(target);
      if (target.url != null) await next.connect(target.url!);
    } on Object catch (error, stack) {
      await next.dispose();
      if (closed) {
        await _restore(previous);
      } else {
        // Still open: give it back what was released from it.
        onInstall?.call(old);
      }
      Error.throwWithStackTrace(error, stack);
    }
    _library = next;
    _profile = target;
    onInstall?.call(next);
  }

  /// Close this profile's library and open it again, as after deleting the
  /// account: the storage is empty, and opening it makes a new device.
  Future<void> reopen() async {
    final old = _library;
    await onRelease?.call(old);
    await old.dispose();
    final next = await _open(_profile);
    _library = next;
    _generation++;
    onInstall?.call(next);
    notifyListeners();
  }

  /// Put [previous] back after a switch failed part-way. Its library has been
  /// closed by then, so it is opened again, with its relay.
  Future<void> _restore(RelayProfile previous) async {
    try {
      await profiles.activate(previous);
    } on Object catch (error) {
      // The selection on disk may still name the target, and the next launch
      // opens that. Its store was opened and is intact, so nothing is lost;
      // this session carries on with the profile it had.
      debugPrint('could not restore the relay selection: $error');
    }
    final restored = await _open(previous);
    _library = restored;
    _profile = previous;
    onInstall?.call(restored);
  }
}
