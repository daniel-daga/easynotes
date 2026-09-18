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

/// A note's colour, as this person chose it.
///
/// A name rather than a `Color`, because the account document holds a name: what
/// "teal" has to look like is a question for the theme of whichever device is
/// drawing it, and the answer differs between daylight and dark mode. The map
/// from one to the other is in the note list screen, where the theme is.
enum NoteColour { red, orange, yellow, green, teal, blue, purple, pink, brown, grey }

/// Whether the note list is a masonry grid or one column. A view setting, and so
/// a personal one: it lives in the account document beside the colours.
enum NoteLayout { grid, list }

/// One note as the grid draws it: enough for a card, without opening the note.
@immutable
class NoteSummary {
  const NoteSummary({
    required this.id,
    required this.title,
    required this.items,
    required this.checked,
    this.preview = const [],
    this.colour,
    this.pinned = false,
    this.archived = false,
    this.trashed = false,
  });

  final String id;
  final String title;

  /// Every item on the list, checked or not.
  final int items;
  final int checked;

  /// The first few items, in display order, so a card can show what the list is
  /// about. Most shopping lists never get a title.
  final List<String> preview;

  /// What this person has done with the note. Personal: it comes from the
  /// account document and is not on the note, so it is not shared with whoever
  /// else is on the list.
  final NoteColour? colour;
  final bool pinned;
  final bool archived;
  final bool trashed;

  /// Whether this note belongs in the grid rather than in the archive or the
  /// trash. The trash wins: a note that was archived and then thrown away is in
  /// one place, not two.
  bool get inGrid => !archived && !trashed;

  @override
  bool operator ==(Object other) =>
      other is NoteSummary &&
      other.id == id &&
      other.title == title &&
      other.items == items &&
      other.checked == checked &&
      other.colour == colour &&
      other.pinned == pinned &&
      other.archived == archived &&
      other.trashed == trashed &&
      listEquals(other.preview, preview);

  @override
  int get hashCode => Object.hash(
    id,
    title,
    items,
    checked,
    Object.hashAll(preview),
    colour,
    pinned,
    archived,
    trashed,
  );

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

  /// Colour a note, or take its colour away. Personal, like everything below it
  /// until [connect].
  Future<void> setColour(String noteId, NoteColour? colour);

  /// Hold a note at the top of the grid, or stop.
  Future<void> setPinned(String noteId, bool pinned);

  /// Put a note out of the way, or bring it back.
  Future<void> setArchived(String noteId, bool archived);

  /// Throw a note away, or take it back out of the trash.
  ///
  /// The note is still on the device and still syncing; what has changed is that
  /// this person does not see it. There is no way to delete one for good yet.
  Future<void> setTrashed(String noteId, bool trashed);

  /// How this person last left the note list.
  NoteLayout get layout;

  Future<void> setLayout(NoteLayout layout);

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
