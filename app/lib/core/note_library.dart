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
enum NoteColour {
  red,
  orange,
  yellow,
  green,
  teal,
  blue,
  purple,
  pink,
  brown,
  grey,
}

/// Whether the note list is a masonry grid or one column. A view setting, and so
/// a personal one: it lives in the account document beside the colours.
enum NoteLayout { grid, list }

/// The longest a label may be.
///
/// The same number the core cuts a name at (`MAX_TAG_LEN` in
/// `core/src/crdt/account.rs`), written twice because a field's `maxLength` has
/// to be a constant here. Being wrong costs a label that is quietly shortened,
/// not a label that is refused, and `a_label_is_cut_at_the_length_the_field_stops_at`
/// in the bridge test holds the two numbers together.
const maxLabelLength = 48;

/// One label on a note: what it is, and what to call it.
///
/// Two fields and not one because the name is the part that can change. A screen
/// draws the `name` and acts on the `id`, so a rename arriving between the two
/// is a redraw rather than a label that cannot be taken off any more.
@immutable
class Label {
  const Label({required this.id, required this.name});

  final String id;
  final String name;

  @override
  bool operator ==(Object other) =>
      other is Label && other.id == id && other.name == name;

  @override
  int get hashCode => Object.hash(id, name);

  @override
  String toString() => 'Label("$name")';
}

/// One label, and how many notes carry it.
///
/// The count is what the filter menu shows beside the name. It can be **zero**:
/// a label exists in its own right rather than only through the notes carrying
/// it, so taking a label off its last note leaves the label. Throwing it away is
/// a separate gesture ([`NoteLibrary.forgetTag`]).
@immutable
class Tag {
  const Tag({required this.id, required this.name, required this.notes});

  final String id;
  final String name;
  final int notes;

  @override
  bool operator ==(Object other) =>
      other is Tag &&
      other.id == id &&
      other.name == name &&
      other.notes == notes;

  @override
  int get hashCode => Object.hash(id, name, notes);

  @override
  String toString() => 'Tag("$name", $notes)';
}

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
    this.tags = const [],
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

  /// The labels on this note, sorted, each once.
  final List<Label> tags;

  /// The IDs of those labels, which is what every question about membership is
  /// asked in. The names are for drawing.
  Set<String> get tagIds => {for (final tag in tags) tag.id};

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
      listEquals(other.preview, preview) &&
      listEquals(other.tags, tags);

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
    Object.hashAll(tags),
  );

  @override
  String toString() => 'NoteSummary($id, "$title", $checked/$items)';
}

/// One note a search found, and the line it was found on.
///
/// The line is here because a card shows four items and a shopping list has
/// twenty: a result with nothing on it to say why it matched reads as a bug
/// rather than as a match.
@immutable
class SearchHit {
  const SearchHit({required this.note, required this.matched});

  final NoteSummary note;

  /// The title, or the item, that the query matched.
  final String matched;

  @override
  bool operator ==(Object other) =>
      other is SearchHit && other.note == note && other.matched == matched;

  @override
  int get hashCode => Object.hash(note, matched);

  @override
  String toString() => 'SearchHit(${note.id}, "$matched")';
}

/// Whether this device is keeping what it is shown, and what went wrong if it
/// is not.
///
/// Its own interface, and a small one, so that it can be handed to a screen
/// that has no business with the rest of a library: a checklist is one note and
/// must not be given the device's other notes, its relay or its trash — but it
/// is where a person is typing, so it is exactly where "this is not being kept"
/// has to appear.
///
/// The question is a real one on one platform only. A device with a disk writes
/// and fsyncs before the edit returns ([ADR
/// 0005](../../../docs/adr/0005-local-store.md)), so there is nothing to report
/// and [storageError] is null for ever. A browser's writes are handed out in
/// batches for Dart to apply ([ADR
/// 0011](../../../docs/adr/0011-the-web-store.md)), the core hands each batch
/// over exactly once and keeps no copy, and so a batch the browser refuses —
/// out of quota, or a database cleared under the tab — is an edit that is gone.
abstract interface class StorageHealth {
  /// What this device could not keep, in the browser's own words, or null while
  /// everything has been kept.
  ///
  /// It latches: once a write has failed it stays failed, because nothing can
  /// undo it. The edit is not in the queue — it was handed over and dropped —
  /// so there is no retry to offer and no later success that would make the
  /// first one true again. Telling someone their notes are safe after telling
  /// them they were not is worse than not telling them at all.
  String? get storageError;

  /// [storageError] now, and every version of it after that. Emits immediately.
  Stream<String?> watchStorageError();
}

abstract class NoteLibrary implements StorageHealth {
  /// The notes right now, newest first, for the first frame.
  List<NoteSummary> get notes;

  /// The list now, and every version of it after that. Emits immediately.
  Stream<List<NoteSummary>> watchNotes();

  /// Make a new, empty note and return its ID.
  Future<String> createNote();

  /// Whether this device can join someone else's list.
  ///
  /// True wherever a relay can be reached, which since the browser transport
  /// landed is everywhere except a tab that has to keep its notes in memory —
  /// there a joined list could be received but not kept, and the tab would lose
  /// it on the next reload. The button is not offered rather than offered and
  /// disappointing.
  bool get canJoin;

  /// Add the note a share blob describes. Returns its ID.
  Future<String> join(String blob);

  /// Every note whose text contains all of the query's words.
  ///
  /// Local, and local in the strong sense: it reads a sealed index this device
  /// keeps, so it works with no network and asks no relay anything. It searches
  /// the whole device, archive and trash included, with each hit carrying the
  /// note's placement — which of them to show is the screen's question, as it is
  /// for the list.
  Future<List<SearchHit>> search(String query);

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

  /// Move a note so that it takes [targetId]'s place in the grid's order.
  ///
  /// What a drag reports, and it names a note rather than a position on purpose:
  /// the screen a drop landed on is filtered to a shelf or a label, and the
  /// order underneath is over every note this device has, so an index counted
  /// off the cards would mean something else by the time it arrived.
  ///
  /// Which side of the target it lands on follows the drag: a note dragged
  /// downwards onto a card ends up after it and one dragged upwards ends up
  /// before it, so the card you dropped on moves out of the way in the
  /// direction you came from. A note dropped on itself changes nothing.
  Future<void> moveNoteTo(String noteId, String targetId);

  /// Every label in use on this device, with how many notes carry each, in the
  /// order a menu wants them.
  ///
  /// Read straight through rather than watched: the note list already carries
  /// every note's labels, so anything drawing this is rebuilding on
  /// [watchNotes] anyway.
  List<Tag> get tags;

  /// Put a label on a note, and return the label it went on under.
  ///
  /// Null for something that is not a name — whitespace, or nothing. What comes
  /// back may not be a new label: a name differing only in case from one this
  /// device already has *is* that label, so the UI must draw what it is told
  /// rather than what was typed.
  Future<String?> addTag(String noteId, String name);

  /// Make a label that no note carries yet, and return it. An existing label
  /// with this name comes back rather than a second one being made.
  Future<String> makeTag(String name);

  /// Take a label off a note. Not an error if it was never on it.
  ///
  /// The label itself stays, with one fewer note on it. [forgetTag] is the one
  /// that throws a label away, and it is a separate gesture on purpose: a label
  /// that vanished because somebody tidied up the one note carrying it is a
  /// thing to have to explain.
  Future<void> removeTag(String noteId, String labelId);

  /// Rename a label. Every note carrying it keeps it.
  ///
  /// Returns the label that survived, which is **not** always the one passed in:
  /// renaming onto a name another label already has merges the two, and the
  /// survivor is the one that already had the name. Null for a name that is not
  /// a name, or a label this device does not have.
  Future<String?> renameTag(String labelId, String name);

  /// Throw a label away: off every note that has it, and gone from the menu.
  /// The notes themselves are untouched.
  Future<void> forgetTag(String labelId);

  /// Put a note out of the way, or bring it back.
  Future<void> setArchived(String noteId, bool archived);

  /// Throw a note away, or take it back out of the trash.
  ///
  /// Reversible, and nothing is destroyed: the note is still on the device and
  /// still syncing, and what has changed is that this person does not see it.
  /// [forgetNote] is the irreversible one.
  Future<void> setTrashed(String noteId, bool trashed);

  /// Delete a note for good: its content, its key and this person's placement of
  /// it all go, and there is no undo.
  ///
  /// It removes **this device's copy**. A note that has been shared is also on
  /// the relay and on every guest's device, under a key they hold, and nothing
  /// here reaches either of those — so the screen must not promise that it does.
  Future<void> forgetNote(String noteId);

  /// Delete every note in the trash for good. Returns how many there were.
  Future<int> emptyTrash();

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
