/// Every note this device holds, and what the UI may do with the set of them.
///
/// The split between this and [NoteCore] is the one the Rust side makes: a
/// library is the device — the list, the relay, the connections — and a core is
/// one note open on one screen. It is the same reason the screens are two: a
/// grid of notes and a checklist want different things and share a document
/// store.
///
/// Like [NoteCore] this is deliberately narrow and one-directional. The list
/// arrives on [watchNotes]; nothing here hands the UI a list it is expected to
/// keep and mutate.
library;

import 'package:flutter/foundation.dart';

import 'note_core.dart';
import 'sync_status.dart';

/// One note as the grid draws it: enough for a card, without opening the note.
@immutable
class NoteSummary {
  const NoteSummary({
    required this.id,
    required this.title,
    required this.items,
    required this.checked,
    this.preview = const [],
  });

  final String id;
  final String title;

  /// Every item on the list, checked or not.
  final int items;
  final int checked;

  /// The first few items, in display order, so a card can show what the list is
  /// about. Most shopping lists never get a title.
  final List<String> preview;

  @override
  bool operator ==(Object other) =>
      other is NoteSummary &&
      other.id == id &&
      other.title == title &&
      other.items == items &&
      other.checked == checked &&
      listEquals(other.preview, preview);

  @override
  int get hashCode =>
      Object.hash(id, title, items, checked, Object.hashAll(preview));

  @override
  String toString() => 'NoteSummary($id, "$title", $checked/$items)';
}

abstract class NoteLibrary {
  /// The notes right now, newest first, for the first frame.
  List<NoteSummary> get notes;

  /// The list now, and every version of it after that. Emits immediately.
  Stream<List<NoteSummary>> watchNotes();

  /// Make a new, empty note and return its ID.
  Future<String> createNote();

  /// Whether this device can join someone else's list.
  ///
  /// False in a browser, which holds its notes in memory and cannot reach a
  /// relay: joining there would add a list that could never receive a word of
  /// its content. The button is not offered rather than offered and useless.
  bool get canJoin;

  /// Add the note a share blob describes. Returns its ID.
  Future<String> join(String blob);

  /// A handle to one note, for a screen that is about to show it.
  ///
  /// The caller disposes what it gets back. Two handles to one note are two
  /// views of one document, not two documents.
  NoteCore openNote(String noteId);

  /// Start syncing every note with the relay at [url], and every note made or
  /// joined after this.
  ///
  /// Returns when the attempt has *started*, not when it has succeeded: there
  /// may be no network, and nothing above this is allowed to wait for one.
  /// [watchSync] is where it says how it went.
  Future<void> connect(String url);

  /// Stop syncing. Edits still work and still queue.
  Future<void> disconnect();

  /// Where this device stands with the relay, over all its notes.
  SyncStatus get syncStatus;

  Stream<SyncStatus> watchSync();

  Future<void> dispose();
}
