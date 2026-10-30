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

  /// Append a section heading: a row with no checkbox that owns the rows under
  /// it until the next one. Returns its ID.
  ///
  /// An older build draws it as an ordinary item with the same words, so
  /// nothing is lost in either direction.
  Future<String> addHeading(String text);

  /// Whether a row is a heading. Returns false when it already was one, which
  /// is how a heading is turned back into an item: the row keeps its ID, its
  /// text and its place, and the rows under it simply join the section above.
  Future<bool> setHeading(String itemId, bool heading);

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

  /// Tidy away a blank item this screen made. The core hides it rather than
  /// deleting it, so text a peer typed into it meanwhile is kept and brings the
  /// row back.
  Future<void> dropIfBlank(String itemId);

  // -- what this person adds over and over ----------------------------------

  /// Remember one line this person committed into a row of this note.
  ///
  /// Called when a row settles rather than on every keystroke: the core cannot
  /// tell a finished line from a half-typed one. It is this device's own
  /// memory — no envelope, nothing for a collaborator or the relay to see —
  /// and it is per note, so one list's history is not a profile of a person.
  Future<void> rememberItem(String text);

  /// The lines to offer back for this note, best first, at most [limit].
  ///
  /// [prefix] is what has been typed so far: empty for a strip of suggestions
  /// beside a blank row, otherwise the half-typed line being completed.
  /// Anything the note already says is left out. Empty when this person has
  /// turned suggestions off, which the library setting holds.
  Future<List<String>> suggestions(String prefix, {int limit = 5});

  /// Forget what this person has added to this note.
  Future<void> forgetHistory();

  /// The headings this device has folded away on this note.
  ///
  /// Folding is local: it is this device's reading of the list, not a fact
  /// about the note, so a partner's phone draws the section open.
  Future<Set<String>> collapsed();

  /// Fold or unfold one section, on this device only.
  Future<void> setCollapsed(String itemId, bool collapsed);

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
