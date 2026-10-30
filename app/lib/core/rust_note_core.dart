/// [NoteCore] backed by the real Rust core through flutter_rust_bridge.
///
/// One note, handed out by [RustNoteLibrary]; the device and its relay are
/// there. The screen talks to [NoteCore]; nothing above this file and its
/// library counterpart imports anything generated, so regenerating the bindings
/// cannot ripple into the UI.
library;

import 'dart:async';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import '../src/rust/api/notes.dart' as rust;
import '../src/rust/frb_generated.dart';
import 'note_core.dart';
import 'note_colour_map.dart';
import 'note_view.dart';
import 'sync_status.dart';

class RustNoteCore implements NoteCore {
  /// Wraps a note handle the library opened. Made by [RustNoteLibrary.open] and
  /// not by anyone else, which is what keeps one core behind every note.
  RustNoteCore(this._note) {
    // Exactly one Rust watcher per note, fanned out to however many widgets
    // want it.
    //
    // Handing the generated stream straight to the UI would be simpler and
    // wrong: a Rust watcher blocks on the view channel, so cancelling a
    // subscription waits for the Rust side to notice, which it only does on the
    // next edit. A widget disposing would hang. The broadcast controller keeps
    // subscribe and unsubscribe on the Dart side, where they are instant, and
    // [dispose] is the one place that has to talk to Rust about it.
    _upstream = _note.watch().listen(
      (view) => _controller.add(_toView(view)),
      onError: _controller.addError,
    );
    // Same reasoning for the sync status: one Rust watcher, fanned out here.
    _syncUpstream = _note.watchSync().listen(
      (status) => _sync.add(toStatus(status)),
      onError: _sync.addError,
    );
  }

  static Future<void>? _initialized;

  /// Load the native library. Safe to call repeatedly: the bridge refuses a
  /// second initialisation, and opening a second note must not bring the app
  /// down.
  ///
  /// [library] is for tests, which load the cdylib from the Cargo target
  /// directory rather than from wherever the bundled app would put it.
  static Future<void> ensureInitialized({ExternalLibrary? library}) =>
      _initialized ??= RustLib.init(externalLibrary: library ?? defaultLibrary);

  /// What [ensureInitialized] loads when the caller names nothing.
  ///
  /// Null in the app, where the bridge's own loader finds the library the
  /// bundle ships. Tests set it to the cdylib in the Cargo target directory, so
  /// that a code path which initialises the bridge *itself* — which is what a
  /// first launch does, through `MasterKeyStore.forDirectory` — can be run
  /// without the test having initialised it first and hidden the thing under
  /// test.
  static ExternalLibrary? defaultLibrary;

  final rust.Note _note;
  final _controller = StreamController<NoteView>.broadcast();
  late final StreamSubscription<rust.NoteView> _upstream;
  final _sync = StreamController<SyncStatus>.broadcast();
  late final StreamSubscription<rust.SyncStatus> _syncUpstream;

  /// Which note this is, so a screen can say what it is showing.
  String get id => _note.id();

  /// Read straight through to Rust rather than caching. A cached copy is only
  /// fresh while something is listening to [watch], and a caller that reads
  /// `current` without subscribing would silently get stale data.
  @override
  NoteView get current => _toView(_note.view());

  @override
  Stream<NoteView> watch() async* {
    yield current;
    yield* _controller.stream;
  }

  @override
  Future<void> setTitle(String title) => _note.setTitle(title: title);

  @override
  Future<int> editBody(String from, String to) =>
      _note.editBody(from: from, to: to);

  @override
  Future<void> setKind(NoteKind kind) => _note.setKind(kind: toRustKind(kind));

  @override
  Future<String> addItem(String text) => _note.addItem(text: text);

  @override
  Future<String> addHeading(String text) => _note.addHeading(text: text);

  @override
  Future<bool> setHeading(String itemId, bool heading) =>
      _note.setHeading(item: itemId, heading: heading);

  @override
  Future<int> setItemText(String itemId, String text) =>
      _note.setItemText(item: itemId, text: text);

  @override
  Future<int> editItemText(String itemId, String from, String to) async {
    try {
      return await _note.editItemText(item: itemId, from: from, to: to);
    } on String catch (message) {
      // A remove can overtake a keystroke already on its way, and the core
      // answers for a row it no longer holds. The note API hands the core's
      // words back as a plain string, so give that one refusal a name the
      // screen can know.
      if (message == 'unknown item $itemId') {
        throw UnknownItemException(itemId);
      }
      rethrow;
    }
  }

  @override
  Future<void> setChecked(String itemId, bool checked) =>
      _note.setChecked(item: itemId, checked: checked);

  @override
  Future<void> moveItem(String itemId, int toIndex) =>
      _note.moveItem(item: itemId, toIndex: toIndex);

  @override
  Future<void> removeItem(String itemId) => _note.removeItem(item: itemId);

  @override
  Future<void> dropIfBlank(String itemId) => _note.dropIfBlank(item: itemId);

  // -- what this person adds over and over ----------------------------------

  @override
  Future<void> rememberItem(String text) =>
      _note.rememberItem(text: text, now: _now());

  @override
  Future<List<String>> suggestions(String prefix, {int limit = 5}) =>
      _note.suggestions(prefix: prefix, limit: limit, now: _now());

  @override
  Future<void> forgetHistory() => _note.forgetHistory();

  @override
  Future<Set<String>> collapsed() async => (await _note.collapsed()).toSet();

  @override
  Future<void> setCollapsed(String itemId, bool collapsed) =>
      _note.setCollapsed(item: itemId, collapsed: collapsed);

  /// Unix seconds, handed to Rust because the core also compiles for the
  /// browser and so has no clock of its own
  /// (`easynotes_core::suggestions`).
  ///
  /// `PlatformInt64Util`, not a bare `int`: the bridge takes an `i64`, which is
  /// an `int` natively and a `BigInt` in a browser.
  static PlatformInt64 _now() =>
      PlatformInt64Util.from(DateTime.now().millisecondsSinceEpoch ~/ 1000);

  @override
  Future<AttachmentView> importAttachment(
    String name,
    Uint8List bytes, {
    Uint8List? thumbnail,
    int? durationMs,
  }) async => toAttachment(
    await _note.importAttachment(
      name: name,
      bytes: bytes,
      thumbnail: thumbnail,
      durationMs: durationMs == null ? null : BigInt.from(durationMs),
    ),
  );

  @override
  Future<AttachmentView> importAttachmentFile(
    String path,
    String name, {
    Uint8List? thumbnail,
    int? durationMs,
  }) async => toAttachment(
    await _note.importAttachmentFile(
      path: path,
      name: name,
      thumbnail: thumbnail,
      durationMs: durationMs == null ? null : BigInt.from(durationMs),
    ),
  );

  @override
  Future<List<AttachmentView>> pendingAttachments() async => [
    for (final a in await _note.pendingAttachments()) toAttachment(a),
  ];

  @override
  Future<Uint8List?> readAttachment(String id, {bool thumbnail = false}) =>
      _note.readAttachment(attachment: id, thumbnail: thumbnail);

  @override
  Future<void> removeAttachment(String id) =>
      _note.removeAttachment(attachment: id);

  @override
  Future<void> moveAttachment(String id, int toIndex) =>
      _note.moveAttachment(attachment: id, toIndex: toIndex);

  @override
  Future<void> describeAttachment(String id, String description) =>
      _note.describeAttachment(attachment: id, description: description);

  @override
  SyncStatus get syncStatus => toStatus(_note.syncStatus());

  @override
  Stream<SyncStatus> watchSync() async* {
    yield syncStatus;
    yield* _sync.stream;
  }

  @override
  Future<void> dispose() async {
    // A screen closing is the end of whatever was being typed on it, so there
    // is no pause left to wait for.
    _note.sendHeldEdits();
    // Stop the Rust watchers: cancelling an upstream subscription waits for that
    // stream to close, and neither closes on its own. The note itself stays open
    // in the library — another screen may be showing it, and the relay keeps
    // syncing it either way.
    await _note.stopWatching();
    await _upstream.cancel();
    await _syncUpstream.cancel();
    await _controller.close();
    await _sync.close();
  }

  /// Shared with [RustNoteLibrary], whose status comes over the same wire.
  ///
  /// `lastSeq` is narrowed here, and this is the only place it could be. The
  /// bridge gives an `i64` as a `PlatformInt64`, which is an `int` natively and
  /// a `BigInt` in a browser, so one Dart `int` field cannot take both — and
  /// widening [SyncStatus] to `PlatformInt64` would put a generated type in the
  /// layer this file exists to keep them out of. Narrowing is safe because the
  /// value is a relay sequence number the app only ever shows: a note would need
  /// nine quadrillion envelopes to reach the edge of a browser's integers, and
  /// `toInt` clamps rather than wraps if one ever did.
  static SyncStatus toStatus(rust.SyncStatus s) => SyncStatus(
    state: switch (s.state) {
      rust.SyncState.empty => SyncState.empty,
      rust.SyncState.offline => SyncState.offline,
      rust.SyncState.connecting => SyncState.connecting,
      rust.SyncState.online => SyncState.online,
      rust.SyncState.synced => SyncState.synced,
    },
    pending: s.pending,
    lastSeq: s.lastSeq.toInt(),
    lastError: s.lastError,
  );

  /// Shared with [RustNoteLibrary], which creates notes of a kind and lists
  /// them with theirs.
  static NoteKind fromRustKind(rust.NoteKindView kind) => switch (kind) {
    rust.NoteKindView.checklist => NoteKind.checklist,
    rust.NoteKindView.text => NoteKind.text,
  };

  static rust.NoteKindView toRustKind(NoteKind kind) => switch (kind) {
    NoteKind.checklist => rust.NoteKindView.checklist,
    NoteKind.text => rust.NoteKindView.text,
  };

  static NoteView _toView(rust.NoteView v) => NoteView(
    rev: v.rev,
    kind: fromRustKind(v.kind),
    title: v.title,
    body: v.body,
    colour: colourFromRust(v.colour),
    colourSet: v.colourSet,
    items: [
      for (final i in v.items)
        ItemView(
          id: i.id,
          text: i.text,
          checked: i.checked,
          order: i.order,
          heading: i.heading,
        ),
    ],
    attachments: [for (final a in v.attachments) toAttachment(a)],
  );

  /// Sizes and durations are `u64` in Rust and a `BigInt` across the bridge;
  /// narrowed here, the one layer that knows. Safe: a file is at most 50 MB.
  static AttachmentView toAttachment(rust.AttachmentView a) => AttachmentView(
    id: a.id,
    kind: switch (a.kind) {
      rust.AttachmentKindView.image => AttachmentKind.image,
      rust.AttachmentKindView.audio => AttachmentKind.audio,
    },
    name: a.name,
    mime: a.mime,
    size: a.size.toInt(),
    width: a.width,
    height: a.height,
    durationMs: a.durationMs?.toInt(),
    description: a.description,
    thumbnailWidth: a.thumbnailWidth,
    thumbnailHeight: a.thumbnailHeight,
    pending: a.pending,
  );
}
