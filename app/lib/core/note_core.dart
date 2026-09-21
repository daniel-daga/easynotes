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

import 'note_view.dart';
import 'sync_status.dart';

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
  Future<void> editBody(String from, String to);

  /// Switch between text and checklist, carrying the content across: each line
  /// with something on it becomes an item, or each item becomes a line. Ticks
  /// do not survive the trip to text. Nothing happens if it is [kind] already.
  Future<void> setKind(NoteKind kind);

  /// Append an item. Returns its ID.
  Future<String> addItem(String text);

  Future<void> setItemText(String itemId, String text);

  Future<void> setChecked(String itemId, bool checked);

  /// Move an item so it lands at [toIndex] in the current display order.
  Future<void> moveItem(String itemId, int toIndex);

  Future<void> removeItem(String itemId);

  /// The blob another device scans or pastes to join this note. In Slice 3 this
  /// becomes the fragment of an invite link, with the same bytes.
  Future<String> exportShareBlob();

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
