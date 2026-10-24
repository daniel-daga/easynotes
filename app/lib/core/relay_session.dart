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

/// Thrown when a person backs out of the account step before an account
/// exists. Nothing has been committed at that point: no profile was selected
/// and no note was uploaded. The setup screen catches it to return to the
/// address step without treating the choice as an error.
class SyncSetupCancelled implements Exception {
  const SyncSetupCancelled();

  @override
  String toString() => 'Sync setup was cancelled.';
}

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
  ///
  /// [access] is the relay's access key, from a relay link or typed in; it is
  /// kept with the target's profile. Given for the relay already open, it is
  /// [setAccess] rather than a switch.
  ///
  /// [connect] is false only while sync is being set up: the profile is
  /// selected, but nothing is uploaded until the account step that goes with
  /// it has finished (ADR 0031). A switch after setup connects normally.
  Future<void> switchTo(
    String? address, {
    String? access,
    bool connect = true,
  }) {
    final running = _switching;
    if (running != null) return running;
    final url = address == null ? null : canonicalRelayUrl(address);
    if (url == _profile.url) {
      if (access != null && url != null && access != _profile.access) {
        return setAccess(access);
      }
      return Future.value();
    }
    final done = _switch(url, access, connect: connect).whenComplete(() {
      _switching = null;
      notifyListeners();
    });
    _switching = done;
    notifyListeners();
    return done;
  }

  Future<void> _switch(
    String? url,
    String? access, {
    required bool connect,
  }) async {
    final target = await profiles.prepare(url, access: access);
    await _select(target, connect: connect);
  }

  /// Select an already-known profile, or put a previous one back after a
  /// setup was abandoned. The order is the same as [switchTo]'s: open the
  /// target before anything is closed, and reopen the old profile if a step
  /// after the close fails.
  Future<void> select(RelayProfile target, {bool connect = true}) {
    final running = _switching;
    if (running != null) return running;
    final done = _select(target, connect: connect).whenComplete(() {
      _switching = null;
      notifyListeners();
    });
    _switching = done;
    notifyListeners();
    return done;
  }

  Future<void> _select(RelayProfile target, {required bool connect}) async {
    final old = _library;
    final previous = _profile;
    if (!old.canJoin) {
      throw StateError(
        'This tab has no storage, so its notes would be lost. Keep it open, '
        'and switch sync servers in a tab that has storage.',
      );
    }
    await old.flush();
    if (old.storageError != null) {
      throw StateError(
        'Some changes here could not be saved. Switching would lose them.',
      );
    }
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
      if (target.url != null && connect) {
        await next.connect(target.url!, access: target.access);
      }
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

  /// Connect the profile in use after its account step has finished. This is
  /// the only place a newly set-up profile starts uploading; the account
  /// already exists by then.
  Future<void> connectCurrent() async {
    final url = _profile.url;
    if (url == null) return;
    await _library.connect(url, access: _profile.access);
  }

  /// Set up sync for [address] as one decision: server, then account, then
  /// connection (ADR 0031).
  ///
  /// [moveNotes] is the first server's "take my notes along" answer. When it
  /// is false and the collection already has notes of its own, the new server
  /// gets a fresh empty profile rather than a copy; the old one stays where
  /// it is. [setUpAccount] shows the account screens for [library] and returns
  /// whether an account now exists. Returning false before one does leaves
  /// everything as it was and throws [SyncSetupCancelled].
  Future<void> connectNewServer(
    String address, {
    String? access,
    required bool moveNotes,
    required Future<bool> Function(NoteLibrary library, RelayProfile profile)
    setUpAccount,
  }) async {
    final url = canonicalRelayUrl(address);
    final saved = profiles.profiles.where((p) => p.url == url).firstOrNull;
    if (saved != null) {
      await switchTo(url, access: access, connect: false);
      if (!_library.hasAccount) {
        final ready = await setUpAccount(_library, _profile);
        if (!ready && !_library.hasAccount) throw const SyncSetupCancelled();
      }
      await connectCurrent();
      return;
    }

    // Reuse the local collection when its notes are coming along, and also
    // when it is empty: a fresh profile for it would only leave an unused
    // "On this device" entry behind.
    final useCurrent =
        moveNotes ||
        (_profile.url == null &&
            _library.notes.isEmpty &&
            !_library.hasAccount);
    if (useCurrent) {
      if (_profile.url != null) {
        throw StateError('Only a local collection can move to a sync server.');
      }
      if (!_library.hasAccount) {
        // The collection has not moved yet, but this is the server it is
        // about to: the account screens should name it, not "no server".
        final about = RelayProfile(id: _profile.id, url: url, access: access);
        final ready = await setUpAccount(_library, about);
        if (!ready && !_library.hasAccount) throw const SyncSetupCancelled();
      }
      await moveLocalToRelay(url, access: access);
      return;
    }

    final previous = _profile;
    await switchTo(url, access: access, connect: false);
    final target = _profile;
    try {
      if (!_library.hasAccount) {
        final ready = await setUpAccount(_library, _profile);
        if (!ready && !_library.hasAccount) throw const SyncSetupCancelled();
      }
    } on Object {
      // Nothing was uploaded: the target is empty and was never connected.
      // Put the previous collection back and forget the cancelled profile.
      await select(previous, connect: previous.url != null);
      await profiles.remove(target);
      rethrow;
    }
    await connectCurrent();
  }

  /// End this profile's server relationship after its account was deleted:
  /// the same collection stays, as the local one, and the address is
  /// forgotten (ADR 0031). A profile with a URL and no account must not be
  /// left behind.
  Future<void> reopenAsLocal() async {
    if (_profile.url != null) {
      _profile = await profiles.clearRelay(_profile);
    }
    await reopen();
  }

  /// Move this device's local collection to an unused relay. No data or key
  /// is copied or deleted: the profile keeps its ID, so native and browser
  /// storage, attachments and any account all stay together. Persist the
  /// selection before connecting, so a restart cannot treat uploaded notes
  /// as a local-only collection. A failed connection leaves this relay
  /// selected and the collection available offline, ready to retry.
  Future<void> moveLocalToRelay(
    String address, {
    String? access,
    bool connect = true,
  }) {
    final running = _switching;
    if (running != null) return running;
    final done = _moveLocalToRelay(address, access, connect: connect)
        .whenComplete(() {
          _switching = null;
          notifyListeners();
        });
    _switching = done;
    notifyListeners();
    return done;
  }

  Future<void> _moveLocalToRelay(
    String address,
    String? access, {
    required bool connect,
  }) async {
    if (_profile.url != null ||
        profiles.profiles.any((p) => p.url == canonicalRelayUrl(address))) {
      throw StateError(
        'Only a local collection can move to a new sync server.',
      );
    }
    if (!_library.canJoin) {
      throw StateError(
        'This tab has no storage. Keep it open and save its notes first.',
      );
    }
    await _library.flush();
    if (_library.storageError != null) {
      throw StateError(
        'Some changes could not be saved. Your notes have not moved.',
      );
    }
    _profile = await profiles.moveLocalToRelay(address, access: access);
    // Tell the app about the durable move even if starting sync throws.
    notifyListeners();
    if (!connect) return;
    try {
      await _library.connect(_profile.url!, access: _profile.access);
    } on Object catch (error) {
      throw StateError(
        'Your collection has moved to ${_profile.url} and is '
        'available offline. Sync could not start: $error',
      );
    }
  }

  /// Keep [access] as this relay's access key, or forget it when null, and
  /// reconnect with it: how a person enters the key a relay asked for, or the
  /// new one after its operator changed it. The same relay and the same
  /// notes; nothing is closed.
  Future<void> setAccess(String? access) async {
    final url = _profile.url;
    if (url == null) {
      throw StateError(
        'Notes on this device have no sync server to give a key to.',
      );
    }
    _profile = await profiles.setAccess(_profile, access);
    await _library.connect(url, access: _profile.access);
    notifyListeners();
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
