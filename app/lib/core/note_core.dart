/// What the UI is allowed to do to a note.
///
/// Deliberately narrow, and deliberately one-directional: every method sends an
/// *edit* and returns nothing useful. The new state arrives on [watch]. A widget
/// that tries to keep its own copy of the list will find it has nothing to keep.
///
/// Two implementations exist: one over the Rust core through
/// flutter_rust_bridge, and [FakeNoteCore] for widget tests. The screen cannot
/// tell them apart, which is why the tests are fast and the screen is testable
/// without a native build.
///
/// One note. The set of them, and the relay they all sync with, is
/// [NoteLibrary].
library;

import 'dart:typed_data';

import 'note_view.dart';
import 'sync_status.dart';

/// A text write's refusal when the item it names is no longer in the note.
///
/// The UI can meet this without anything being wrong: a write may still be on
/// its way when the row it was typed in is removed, as holding backspace past
/// empty does. A caller that can tell it raced a removal treats this as "the
/// row is already gone" and drops the write.
class UnknownItemException implements Exception {
  const UnknownItemException(this.itemId);

  final String itemId;

  @override
  String toString() => 'unknown item $itemId';
}

abstract class NoteCore {
  /// The current view, then every subsequent one. Emits immediately on listen.
  Stream<NoteView> watch();

  /// The view right now, for the initial frame before the stream fires.
  NoteView get current;

  Future<void> setTitle(String title);

  /// Say that this person changed the body from [from] — what the screen was
  /// showing — to [to]. The core replays that change onto the body as it now
  /// stands, so a peer's edit that arrived after the screen last drew is kept
  /// rather than typed over. Only the change travels.
  ///
  /// Returns the [NoteView.rev] the edit landed at, so the field that made it
  /// can tell its own echo from somebody else's change.
  Future<int> editBody(String from, String to);

  /// Switch between text and checklist, carrying the content across: each line
  /// with something on it becomes an item, or each item becomes a line. Ticks
  /// do not survive the trip to text. Nothing happens if it is [kind] already.
  Future<void> setKind(NoteKind kind);

  /// Append an item. Returns its ID.
  Future<String> addItem(String text);

  /// Set an item's text. Returns the [NoteView.rev] the edit landed at, so the
  /// field that made it can tell its own echo from somebody else's change.
  Future<int> setItemText(String itemId, String text);

  /// Set an item's text as the field's own edit: the text it was showing and
  /// the text it shows now. Replayed onto whatever the item says by the time it
  /// arrives, so typing in the same item as somebody else keeps both people's
  /// words. Returns the revision it landed at, like [setItemText].
  Future<int> editItemText(String itemId, String from, String to);

  Future<void> setChecked(String itemId, bool checked);

  /// Move an item so it lands at [toIndex] in the current display order.
  Future<void> moveItem(String itemId, int toIndex);

  Future<void> removeItem(String itemId);

  // -- attachments ----------------------------------------------------------

  /// Attach a file handed over whole: a tab's picked file, or a photo in
  /// memory. [thumbnail] is a preview of at most 512 pixels a side that the
  /// app made; [durationMs] is a sound file's length, if the platform knows
  /// it. The file is saved on this device when this returns, and is one of
  /// [pendingAttachments] until it is in the note.
  Future<AttachmentView> importAttachment(
    String name,
    Uint8List bytes, {
    Uint8List? thumbnail,
    int? durationMs,
  });

  /// The same for a file on this device's disk, read a chunk at a time. Not
  /// on the web, which has no paths.
  Future<AttachmentView> importAttachmentFile(
    String path,
    String name, {
    Uint8List? thumbnail,
    int? durationMs,
  });

  /// This device's imports into this note that are not in it yet.
  Future<List<AttachmentView>> pendingAttachments();

  /// An attachment's bytes — its thumbnail, or the whole file — or null while
  /// they are not on this device, in which case they are asked for and
  /// [NoteLibrary.watchTransfers] follows them.
  Future<Uint8List?> readAttachment(String id, {bool thumbnail = false});

  /// Remove an attachment for everyone on the note, or give up on an import
  /// that is not in it yet.
  Future<void> removeAttachment(String id);

  /// Move an attachment so it lands at [toIndex] among the note's.
  Future<void> moveAttachment(String id, int toIndex);

  Future<void> describeAttachment(String id, String description);

  /// The sync status of this note now, and every change to it.
  ///
  /// Connecting is not here: what to sync with is a decision for the device,
  /// not for one note, so it lives on [NoteLibrary]. This is only where a note
  /// says how its own share of that is going.
  Stream<SyncStatus> watchSync();

  /// The status right now, for the first frame.
  SyncStatus get syncStatus;

  /// Stop watching and release whatever the implementation holds.
  Future<void> dispose();
}
