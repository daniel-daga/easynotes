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
library;

import 'note_view.dart';
import 'sync_status.dart';

abstract class NoteCore {
  /// The current view, then every subsequent one. Emits immediately on listen.
  Stream<NoteView> watch();

  /// The view right now, for the initial frame before the stream fires.
  NoteView get current;

  Future<void> setTitle(String title);

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

  /// Start syncing with the relay at [url].
  ///
  /// Returns when the attempt has *started*, not when it has succeeded: there
  /// may be no network, and nothing above this is allowed to wait for one.
  /// [watchSync] is where it says how it went.
  Future<void> connect(String url);

  /// Stop syncing. Edits still work and still queue.
  Future<void> disconnect();

  /// The sync status now, and every change to it.
  Stream<SyncStatus> watchSync();

  /// The status right now, for the first frame.
  SyncStatus get syncStatus;

  /// Stop watching and release whatever the implementation holds.
  Future<void> dispose();
}
