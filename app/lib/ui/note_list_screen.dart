/// The screen the app opens on: every note this device holds.
///
/// It obeys the same rule as the checklist. There is one `StreamBuilder` over
/// the library's note list and everything below it is a function of what that
/// yields — a note created here, a note joined, a note someone else edited on
/// another device, and a note this person has just coloured or pinned all reach
/// the grid by the same path, because the core refreshes the list for all of
/// them.
///
/// The layout is Keep's: a masonry grid of cards, with a toggle to a single
/// column, and three shelves — the grid, the archive and the trash. Which
/// layout you chose is a view setting and which shelf a note is on is a
/// placement; both are personal, both live in the account document, and both
/// therefore outlive the screen.
library;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';

import '../core/note_library.dart';
import 'checklist_screen.dart';
import 'storage_banner.dart';
import 'sync_button.dart';

/// Where a note can be: in the grid, out of the way, or thrown away.
///
/// The list the library hands over holds all three, because filtering is a
/// question about a screen and not about a device. This is the filter.
enum _Shelf {
  notes('EasyNotes', 'No lists yet. Make one.'),
  archive('Archive', 'Nothing is archived.'),
  trash('Trash', 'The trash is empty.');

  const _Shelf(this.title, this.empty);

  final String title;
  final String empty;

  bool holds(NoteSummary note) => switch (this) {
    _Shelf.notes => note.inGrid,
    // The trash wins over the archive, so a note in both is only in the trash.
    _Shelf.archive => note.archived && !note.trashed,
    _Shelf.trash => note.trashed,
  };
}

/// What the screen is showing: one of the three shelves, or one label.
///
/// A label is a fourth shelf rather than a filter inside the other three. That
/// is Keep's shape, and it is the one that needs no extra control: "the archived
/// notes labelled errands" is a question nobody has wanted a button for, and
/// giving a label its own shelf keeps the drawer a list of places rather than a
/// place and a modifier.
@immutable
class _Where {
  const _Where.shelf(this.shelf) : tag = null;

  /// A label's shelf is the grid, which is what makes a new note made here land
  /// somewhere this person can see it.
  const _Where.tag(String this.tag) : shelf = _Shelf.notes;

  final _Shelf shelf;
  final String? tag;

  String get title => tag ?? shelf.title;

  String get empty =>
      tag == null ? shelf.empty : 'Nothing here is labelled "$tag".';

  /// Whether this is the plain grid, where a list can be joined.
  bool get isGrid => tag == null && shelf == _Shelf.notes;

  /// Whether a new note made here would be visible afterwards. True on the grid
  /// and on a label — where the note is given that label, so it does not appear
  /// and then vanish.
  bool get takesNewNotes => shelf == _Shelf.notes;

  bool holds(NoteSummary note) => tag == null
      ? shelf.holds(note)
      // A note keeps its labels in the archive and in the trash; what a label
      // shows is what is on the shelf this person actually looks at.
      : note.inGrid && note.tags.contains(tag);

  @override
  bool operator ==(Object other) =>
      other is _Where && other.shelf == shelf && other.tag == tag;

  @override
  int get hashCode => Object.hash(shelf, tag);
}

class NoteListScreen extends StatefulWidget {
  const NoteListScreen({super.key, required this.library});

  final NoteLibrary library;

  @override
  State<NoteListScreen> createState() => _NoteListScreenState();
}

class _NoteListScreenState extends State<NoteListScreen> {
  /// What this person last chose, which the library remembers for them.
  late NoteLayout _layout = widget.library.layout;
  _Where _where = const _Where.shelf(_Shelf.notes);

  /// The search box, when it is open, and what is in it.
  ///
  /// Not remembered the way the layout is: a colour is a decision about a note
  /// and a search is a question about right now, so it goes away with the
  /// screen. The query is held here as well as in the controller because the
  /// body is rebuilt from it and a controller is not a source of truth.
  final _search = TextEditingController();
  bool _searching = false;
  String _query = '';

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  void _openSearch() => setState(() => _searching = true);

  void _closeSearch() {
    _search.clear();
    setState(() {
      _searching = false;
      _query = '';
    });
  }

  /// Open a note, and hold the handle open only while its screen is on top.
  ///
  /// The library keeps the note itself open and syncing; what this disposes is
  /// the watcher behind the screen that has just gone away. Leaking one per
  /// visit would leave a Rust task per note per open, for ever.
  Future<void> _openNote(String id) async {
    final core = widget.library.openNote(id);
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (context) =>
            ChecklistScreen(core: core, storage: widget.library),
      ),
    );
    await core.dispose();
  }

  Future<void> _createNote() async {
    final id = await widget.library.createNote();
    // Made while looking at a label, so it gets that label: otherwise the note
    // would be created, drawn for a frame, and gone from the shelf it was made
    // on as soon as the list refreshed.
    final tag = _where.tag;
    if (tag != null) await widget.library.addTag(id, tag);
    if (!mounted) return;
    await _openNote(id);
  }

  /// The labels on one note, on a dialog that can put more on and take them off.
  ///
  /// Shown with the screen's context rather than the action sheet's, for the
  /// reason the delete confirmation is: the sheet is popped before this opens.
  Future<void> _labelsFor(NoteSummary note) async {
    await showDialog<void>(
      context: context,
      builder: (context) => _LabelsDialog(library: widget.library, note: note),
    );
  }

  /// Show one label's notes, or go back to a shelf. Either way the drawer is the
  /// only way in, so it closes itself.
  void _show(_Where where) => setState(() => _where = where);

  Future<void> _join() async {
    final blob = await showDialog<String>(
      context: context,
      builder: (context) => const _JoinDialog(),
    );
    if (blob == null || blob.isEmpty || !mounted) return;
    final String id;
    try {
      id = await widget.library.join(blob);
    } catch (e) {
      if (!mounted) return;
      // The blob is pasted by hand, so a bad one is an ordinary thing to do and
      // not a crash. Its own words, because "invalid" would not help anyone fix
      // a truncated paste.
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('$e', key: const Key('join-error'))),
      );
      return;
    }
    if (!mounted) return;
    await _openNote(id);
  }

  Future<void> _toggleLayout() async {
    final next = _layout == NoteLayout.grid ? NoteLayout.list : NoteLayout.grid;
    setState(() => _layout = next);
    await widget.library.setLayout(next);
  }

  /// Delete one note for good, if this person says so a second time.
  ///
  /// The confirmation is not ceremony. It is the only thing between a long press
  /// and a list nobody can get back, and it is where the promise the app cannot
  /// keep gets said out loud: deleting a shared list here does not delete it for
  /// whoever it was shared with, because their copy is theirs and under a key
  /// they hold.
  Future<void> _forget(NoteSummary note) async {
    final title = note.title.trim();
    final confirmed = await _confirmDelete(
      question: title.isEmpty
          ? 'Delete this list for good?'
          : 'Delete "$title" for good?',
      action: 'Delete',
    );
    if (!confirmed || !mounted) return;
    await widget.library.forgetNote(note.id);
  }

  /// The same, for everything in the trash at once.
  Future<void> _emptyTrash(int count) async {
    final confirmed = await _confirmDelete(
      question: count == 1
          ? 'Delete 1 list for good?'
          : 'Delete all $count lists for good?',
      action: 'Delete all',
    );
    if (!confirmed || !mounted) return;
    await widget.library.emptyTrash();
  }

  Future<bool> _confirmDelete({
    required String question,
    required String action,
  }) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        key: const Key('confirm-delete'),
        title: Text(question),
        // Worded so that it reads the same for one list and for a trash full of
        // them, and so that the second sentence is there at all: a person
        // deleting a shared shopping list may well think they are deleting it
        // for the household, and they are not.
        content: const Text(
          'Deleted from this device, and there is no undo. '
          'Anything you shared stays with whoever you shared it with.',
        ),
        actions: [
          TextButton(
            key: const Key('confirm-keep'),
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Keep'),
          ),
          TextButton(
            key: const Key('confirm-delete-forever'),
            style: TextButton.styleFrom(
              foregroundColor: Theme.of(context).colorScheme.error,
            ),
            onPressed: () => Navigator.of(context).pop(true),
            child: Text(action),
          ),
        ],
      ),
    );
    // A dialog dismissed by tapping outside it returns null, and the answer to
    // "shall I delete this?" that nobody gave is no.
    return confirmed ?? false;
  }

  /// The actions for one note, on a sheet that comes up from the bottom.
  ///
  /// A long press rather than a button on every card: a card is mostly the
  /// note, and nine of ten visits to this screen are to read one or open one.
  Future<void> _actionsFor(NoteSummary note) async {
    await showModalBottomSheet<void>(
      context: context,
      builder: (context) => _NoteActions(
        note: note,
        library: widget.library,
        onLabels: () => _labelsFor(note),
        // Handed down rather than called from the sheet: the confirmation
        // outlives the sheet that asked for it, so it has to be shown with the
        // screen's context and not with one that is about to be popped.
        onForget: () => _forget(note),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<List<NoteSummary>>(
      stream: widget.library.watchNotes(),
      initialData: widget.library.notes,
      builder: (context, snapshot) {
        final all = snapshot.data ?? const <NoteSummary>[];
        final notes = all.where(_where.holds).toList();
        // Read here rather than in the drawer, so that a label appearing or
        // disappearing follows the same rebuild every other change to the list
        // does. A label's whole existence is the notes that carry it.
        final tags = widget.library.tags;
        // The label this person is looking at, gone: its last note was
        // relabelled, trashed or deleted. Back to the grid, because a shelf that
        // does not exist cannot say anything about why it is empty.
        final tag = _where.tag;
        if (tag != null && !tags.any((held) => held.name == tag)) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted) _show(const _Where.shelf(_Shelf.notes));
          });
        }
        return Scaffold(
          appBar: AppBar(
            leading: _searching
                ? IconButton(
                    key: const Key('search-close'),
                    icon: const Icon(Icons.arrow_back),
                    tooltip: 'Stop searching',
                    onPressed: _closeSearch,
                  )
                : null,
            title: _searching
                ? TextField(
                    key: const Key('search-field'),
                    controller: _search,
                    autofocus: true,
                    decoration: const InputDecoration(
                      hintText: 'Search your lists',
                      border: InputBorder.none,
                    ),
                    onChanged: (value) => setState(() => _query = value),
                  )
                : Text(_where.title),
            actions: [
              if (!_searching) ...[
                SyncStatusButton(
                  stream: widget.library.watchSync(),
                  initial: widget.library.syncStatus,
                ),
                IconButton(
                  key: const Key('search-button'),
                  icon: const Icon(Icons.search),
                  tooltip: 'Search',
                  onPressed: _openSearch,
                ),
                IconButton(
                  key: const Key('layout-toggle'),
                  icon: Icon(
                    _layout == NoteLayout.grid
                        ? Icons.view_agenda_outlined
                        : Icons.grid_view,
                  ),
                  tooltip: _layout == NoteLayout.grid
                      ? 'Show as a list'
                      : 'Show as a grid',
                  onPressed: _toggleLayout,
                ),
                if (widget.library.canJoin && _where.isGrid)
                  IconButton(
                    key: const Key('join-button'),
                    icon: const Icon(Icons.group_add_outlined),
                    tooltip: 'Join a list',
                    onPressed: _join,
                  ),
                // Only where there is a trash and something in it. An empty
                // trash with a button to empty it invites the one gesture that
                // cannot be undone, for no result.
                if (_where.shelf == _Shelf.trash && notes.isNotEmpty)
                  IconButton(
                    key: const Key('empty-trash-button'),
                    icon: const Icon(Icons.delete_sweep_outlined),
                    tooltip: 'Empty the trash',
                    onPressed: () => _emptyTrash(notes.length),
                  ),
              ],
            ],
          ),
          drawer: _Shelves(
            current: _where,
            counts: {
              for (final shelf in _Shelf.values)
                shelf: all.where(shelf.holds).length,
            },
            tags: tags,
            onChosen: _show,
          ),
          // Only where a new note would land. One made while looking at the
          // trash would appear in a grid that is not on screen, and one made
          // while searching would appear in results it does not match.
          floatingActionButton: _where.takesNewNotes && !_searching
              ? FloatingActionButton(
                  key: const Key('new-note-button'),
                  tooltip: 'New list',
                  onPressed: _createNote,
                  child: const Icon(Icons.add),
                )
              : null,
          // The banner above whatever is on screen, rather than beside the
          // sync button: this is not a state to glance at, and a person who has
          // lost an edit should not have to notice an icon to find out.
          body: Column(
            children: [
              StorageBanner(storage: widget.library),
              Expanded(child: _body(all, notes)),
            ],
          ),
        );
      },
    );
  }

  /// The grid, the results or the empty line, whichever this screen is showing.
  Widget _body(List<NoteSummary> all, List<NoteSummary> notes) {
    // The shelf still decides what is on screen: searching the trash searches
    // the trash. The library answers for the whole device and this is where
    // that is narrowed, exactly as the list is.
    return _query.trim().isNotEmpty
        ? _SearchResults(
            library: widget.library,
            query: _query.trim(),
            where: _where,
            // Changes when anything about the notes changes, which is what
            // makes a result follow an edit someone else just made.
            notes: all,
            layout: _layout,
            onTap: _openNote,
            onHold: _actionsFor,
          )
        : notes.isEmpty
        ? Center(key: const Key('no-notes'), child: Text(_where.empty))
        : _NoteCollection(
            notes: notes,
            layout: _layout,
            onTap: _openNote,
            onHold: _actionsFor,
            onMove: _moveNote,
          );
  }

  /// A card was dropped on another. The library is told which two notes they
  /// were and works out the order key; nothing here counts positions, because
  /// what is on screen is a shelf or a label and the order is over the lot.
  void _moveNote(String id, String targetId) {
    widget.library.moveNoteTo(id, targetId);
  }
}

/// What the library found, drawn as the same cards with the matching line on
/// them.
///
/// A widget of its own because searching is the one thing on this screen that
/// is not a function of the note list: the text is inside the notes and only
/// the library can read it, so this asks and waits. It asks again whenever the
/// query changes *or* the list does, which is what makes a result follow an edit
/// arriving from another device while the results are on screen.
class _SearchResults extends StatefulWidget {
  const _SearchResults({
    required this.library,
    required this.query,
    required this.where,
    required this.notes,
    required this.layout,
    required this.onTap,
    required this.onHold,
  });

  final NoteLibrary library;
  final String query;
  final _Where where;
  final List<NoteSummary> notes;
  final NoteLayout layout;
  final void Function(String id) onTap;
  final void Function(NoteSummary note) onHold;

  @override
  State<_SearchResults> createState() => _SearchResultsState();
}

class _SearchResultsState extends State<_SearchResults> {
  List<SearchHit>? _hits;

  /// Which search is the current one. Typing is faster than a search returns,
  /// and without this the answer to "aspar" can arrive after the answer to
  /// "asparagus" and be shown instead of it.
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _run();
  }

  @override
  void didUpdateWidget(_SearchResults old) {
    super.didUpdateWidget(old);
    if (old.query != widget.query || !listEquals(old.notes, widget.notes)) {
      _run();
    }
  }

  Future<void> _run() async {
    final generation = ++_generation;
    final hits = await widget.library.search(widget.query);
    if (!mounted || generation != _generation) return;
    setState(() => _hits = hits);
  }

  @override
  Widget build(BuildContext context) {
    final hits = _hits;
    // The first frame, before the first answer. Blank rather than a spinner: a
    // local search over a household's lists is done well inside a frame, and a
    // spinner that flashes says something untrue about what is happening.
    if (hits == null) return const SizedBox.shrink();

    final found = hits.where((hit) => widget.where.holds(hit.note)).toList();
    if (found.isEmpty) {
      return Center(
        key: const Key('no-matches'),
        child: Text('Nothing here matches "${widget.query}".'),
      );
    }
    return _NoteCollection(
      notes: [for (final hit in found) hit.note],
      matched: {for (final hit in found) hit.note.id: hit.matched},
      layout: widget.layout,
      onTap: widget.onTap,
      onHold: widget.onHold,
    );
  }
}

/// The grid, or the single column. Both draw the same cards.
class _NoteCollection extends StatelessWidget {
  const _NoteCollection({
    required this.notes,
    required this.layout,
    required this.onTap,
    required this.onHold,
    this.onMove,
    this.matched = const {},
  });

  final List<NoteSummary> notes;
  final NoteLayout layout;
  final void Function(String id) onTap;
  final void Function(NoteSummary note) onHold;

  /// Where a card was dropped: the note that moved, and the note it landed on.
  ///
  /// Null on the one collection that cannot be reordered — search results, where
  /// the cards on screen are the answer to a question rather than the grid, and
  /// dropping one on another would move two notes past each other in an order
  /// most of which is not being shown.
  final void Function(String id, String targetId)? onMove;

  /// The line a search matched, per note. Empty when this is the plain list.
  final Map<String, String> matched;

  @override
  Widget build(BuildContext context) {
    final move = onMove;
    final cards = [
      for (final note in notes)
        _NoteCard(
          key: ValueKey(note.id),
          note: note,
          matched: matched[note.id],
          onTap: () => onTap(note.id),
          // Held by the wrapper where there is one, because a long press there
          // is the start of a drag and only turns into this when nothing moved.
          onHold: move == null ? () => onHold(note) : null,
        ),
    ];
    Widget wrap(_NoteCard card) => move == null
        ? card
        : _DraggableCard(
            card: card,
            onMove: move,
            onHold: () => onHold(card.note),
          );
    return layout == NoteLayout.grid
        ? _MasonryGrid(wrap: wrap, children: cards)
        : ListView(
            padding: const EdgeInsets.all(8),
            children: [
              for (final card in cards)
                Padding(
                  padding: const EdgeInsets.only(bottom: 8),
                  child: wrap(card),
                ),
            ],
          );
  }
}

/// A card that can be picked up, and that a picked-up card can be dropped on.
///
/// The gesture is Keep's, and it has to do two jobs with one press: holding a
/// card lifts it, and letting go without having moved it opens the card's menu.
/// They cannot be two gestures — a long press that both starts a drag and opens
/// a sheet is one pointer in two places — so the drag is the gesture and the
/// menu is what a drag that went nowhere means.
///
/// A drop is refused across the pin line. Pinned notes are drawn first but share
/// one order with the rest ([ADR 0008](../../../docs/adr/0008-account-document.md)),
/// so dragging a pinned card among the unpinned ones would write a move that
/// nothing on screen could show — the card would spring back to the top and look
/// broken. Unpin it and it lands where it was dropped.
class _DraggableCard extends StatefulWidget {
  const _DraggableCard({
    required this.card,
    required this.onMove,
    required this.onHold,
  });

  final _NoteCard card;
  final void Function(String id, String targetId) onMove;
  final VoidCallback onHold;

  @override
  State<_DraggableCard> createState() => _DraggableCardState();
}

class _DraggableCardState extends State<_DraggableCard> {
  /// How far the pointer has travelled since the card was picked up, and so
  /// whether this is a drag or a hand resting on a screen.
  ///
  /// The path's length rather than the distance from where it started: a card
  /// dragged out and back is still a card that was dragged, and a first update
  /// that already carries the whole movement — which is every drag a test makes
  /// — has no earlier position to be measured against.
  double _travelled = 0;
  bool _moved = false;

  NoteSummary get _note => widget.card.note;

  void _started() {
    _travelled = 0;
    _moved = false;
  }

  void _updated(DragUpdateDetails details) {
    _travelled += details.delta.distance;
    if (_travelled > kTouchSlop) _moved = true;
  }

  void _ended(DraggableDetails details) {
    // Dropped on nothing, having gone nowhere: the press was a press. A drag
    // that travelled and was refused — across the pin line, or onto empty
    // space — is a drag that missed, and opening a menu under the finger that
    // was aiming somewhere else would be a surprise.
    if (!details.wasAccepted && !_moved) widget.onHold();
  }

  @override
  Widget build(BuildContext context) {
    return DragTarget<NoteSummary>(
      onWillAcceptWithDetails: (details) =>
          details.data.id != _note.id && details.data.pinned == _note.pinned,
      onAcceptWithDetails: (details) =>
          widget.onMove(details.data.id, _note.id),
      builder: (context, candidates, _) => LayoutBuilder(
        builder: (context, constraints) => Container(
          // `foregroundDecoration`, so that the outline is painted over the card
          // rather than around it: a border that took up space would move every
          // card below it the moment a drag passed over this one.
          foregroundDecoration: candidates.isEmpty
              ? null
              : BoxDecoration(
                  borderRadius: BorderRadius.circular(12),
                  border: Border.all(
                    color: Theme.of(context).colorScheme.primary,
                    width: 2,
                  ),
                ),
          child: LongPressDraggable<NoteSummary>(
            data: _note,
            onDragStarted: _started,
            onDragUpdate: _updated,
            onDragEnd: _ended,
            feedback: SizedBox(
              width: constraints.maxWidth,
              child: Opacity(
                opacity: 0.9,
                child: Material(
                  type: MaterialType.transparency,
                  child: widget.card,
                ),
              ),
            ),
            // The gap the card came out of, so the columns do not close up
            // underneath the finger holding it.
            childWhenDragging: Opacity(
              opacity: 0.3,
              child: IgnorePointer(child: widget.card),
            ),
            child: widget.card,
          ),
        ),
      ),
    );
  }
}

/// The three shelves and this person's labels, with how many notes are on each.
class _Shelves extends StatelessWidget {
  const _Shelves({
    required this.current,
    required this.counts,
    required this.tags,
    required this.onChosen,
  });

  final _Where current;
  final Map<_Shelf, int> counts;

  /// Every label in use, in the order the library gives them. Empty until this
  /// person makes one, and empty again when they take the last one off — there is
  /// no list of labels apart from the notes that carry them.
  final List<Tag> tags;
  final void Function(_Where) onChosen;

  static const _icons = {
    _Shelf.notes: Icons.lightbulb_outline,
    _Shelf.archive: Icons.archive_outlined,
    _Shelf.trash: Icons.delete_outline,
  };

  @override
  Widget build(BuildContext context) {
    void go(_Where where) {
      Navigator.of(context).pop();
      onChosen(where);
    }

    return Drawer(
      child: ListView(
        padding: EdgeInsets.zero,
        children: [
          const DrawerHeader(child: Text('EasyNotes')),
          for (final shelf in _Shelf.values)
            ListTile(
              key: Key('shelf-${shelf.name}'),
              leading: Icon(_icons[shelf]),
              title: Text(shelf == _Shelf.notes ? 'Notes' : shelf.title),
              trailing: Text('${counts[shelf] ?? 0}'),
              selected: current.tag == null && shelf == current.shelf,
              onTap: () => go(_Where.shelf(shelf)),
            ),
          // No heading over an empty list: a person who has never made a label
          // has nothing to be told about them here, and the place labels are
          // made is a note.
          if (tags.isNotEmpty) ...[
            const Divider(),
            Padding(
              padding: const EdgeInsets.only(left: 16, bottom: 4),
              child: Text(
                'Labels',
                style: Theme.of(context).textTheme.labelMedium,
              ),
            ),
            for (final tag in tags)
              ListTile(
                key: Key('label-shelf-${tag.name}'),
                leading: const Icon(Icons.label_outline),
                title: Text(tag.name),
                trailing: Text('${tag.notes}'),
                selected: current.tag == tag.name,
                onTap: () => go(_Where.tag(tag.name)),
              ),
          ],
        ],
      ),
    );
  }
}

/// What can be done to one note, without opening it.
class _NoteActions extends StatelessWidget {
  const _NoteActions({
    required this.note,
    required this.library,
    required this.onLabels,
    required this.onForget,
  });

  final NoteSummary note;
  final NoteLibrary library;

  /// Ask the screen for the label dialog. Handed down for the reason [onForget]
  /// is: it outlives this sheet, which is popped before it opens.
  final Future<void> Function() onLabels;

  /// Ask the screen to delete this note for good. Not `library.forgetNote`
  /// directly: it has to be confirmed first, and the confirmation is shown after
  /// this sheet has gone.
  final Future<void> Function() onForget;

  @override
  Widget build(BuildContext context) {
    void run(Future<void> Function() action) {
      Navigator.of(context).pop();
      action();
    }

    return SafeArea(
      key: const Key('note-actions'),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          SizedBox(
            height: 64,
            child: ListView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 12),
              children: [
                _Swatch(
                  colour: null,
                  chosen: note.colour == null,
                  onTap: () => run(() => library.setColour(note.id, null)),
                ),
                for (final colour in NoteColour.values)
                  _Swatch(
                    colour: colour,
                    chosen: note.colour == colour,
                    onTap: () => run(() => library.setColour(note.id, colour)),
                  ),
              ],
            ),
          ),
          const Divider(height: 1),
          if (!note.trashed)
            ListTile(
              key: const Key('action-pin'),
              leading: Icon(
                note.pinned ? Icons.push_pin : Icons.push_pin_outlined,
              ),
              title: Text(note.pinned ? 'Unpin' : 'Pin'),
              onTap: () => run(() => library.setPinned(note.id, !note.pinned)),
            ),
          if (!note.trashed)
            ListTile(
              key: const Key('action-labels'),
              leading: const Icon(Icons.label_outline),
              title: const Text('Labels'),
              subtitle: note.tags.isEmpty
                  ? null
                  : Text(note.tags.join(', '), maxLines: 1),
              onTap: () => run(onLabels),
            ),
          if (!note.trashed)
            ListTile(
              key: const Key('action-archive'),
              leading: Icon(
                note.archived
                    ? Icons.unarchive_outlined
                    : Icons.archive_outlined,
              ),
              title: Text(note.archived ? 'Unarchive' : 'Archive'),
              onTap: () =>
                  run(() => library.setArchived(note.id, !note.archived)),
            ),
          ListTile(
            key: const Key('action-trash'),
            leading: Icon(
              note.trashed ? Icons.restore_from_trash : Icons.delete_outline,
            ),
            title: Text(note.trashed ? 'Restore' : 'Move to trash'),
            // Nothing is deleted: the note stays on the device and keeps
            // syncing, and what changes is that this person stops seeing it.
            subtitle: note.trashed
                ? null
                : const Text('Kept, and out of the grid'),
            onTap: () => run(() => library.setTrashed(note.id, !note.trashed)),
          ),
          // Only from the trash. Throwing a note away and deleting it for good
          // are one gesture apart everywhere else, and one of them cannot be
          // taken back — so the reversible one has to happen first.
          if (note.trashed)
            ListTile(
              key: const Key('action-delete-forever'),
              leading: Icon(
                Icons.delete_forever_outlined,
                color: Theme.of(context).colorScheme.error,
              ),
              title: Text(
                'Delete for good',
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
              subtitle: const Text('Gone from this device, with no undo'),
              onTap: () => run(onForget),
            ),
        ],
      ),
    );
  }
}

class _Swatch extends StatelessWidget {
  const _Swatch({
    required this.colour,
    required this.chosen,
    required this.onTap,
  });

  final NoteColour? colour;
  final bool chosen;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 8),
      child: InkWell(
        key: Key('swatch-${colour?.name ?? 'none'}'),
        customBorder: const CircleBorder(),
        onTap: onTap,
        child: Container(
          width: 44,
          height: 44,
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            color: cardColour(context, colour) ?? theme.colorScheme.surface,
            border: Border.all(
              color: chosen ? theme.colorScheme.primary : theme.dividerColor,
              width: chosen ? 3 : 1,
            ),
          ),
          child: colour == null
              ? Icon(Icons.format_color_reset_outlined, size: 20)
              : null,
        ),
      ),
    );
  }
}

/// What a note's colour looks like on this device, in this theme.
///
/// The account document holds a *name*, because a colour has to mean something
/// on a phone in the dark and on a desktop in daylight and cannot be sent as a
/// number. The name is turned into a tint of the surface, so a card is legible
/// in either theme with the same text colour on it.
@visibleForTesting
Color? cardColour(BuildContext context, NoteColour? colour) {
  if (colour == null) return null;
  final scheme = Theme.of(context).colorScheme;
  final base = switch (colour) {
    NoteColour.red => const Color(0xFFE53935),
    NoteColour.orange => const Color(0xFFF57C00),
    NoteColour.yellow => const Color(0xFFFDD835),
    NoteColour.green => const Color(0xFF43A047),
    NoteColour.teal => const Color(0xFF00897B),
    NoteColour.blue => const Color(0xFF1E88E5),
    NoteColour.purple => const Color(0xFF8E24AA),
    NoteColour.pink => const Color(0xFFD81B60),
    NoteColour.brown => const Color(0xFF6D4C41),
    NoteColour.grey => const Color(0xFF757575),
  };
  // A tint rather than the colour itself: Keep's cards are pale, and a card that
  // is fully saturated needs its own text colour to stay readable.
  final strength = scheme.brightness == Brightness.dark ? 0.32 : 0.22;
  return Color.alphaBlend(base.withValues(alpha: strength), scheme.surface);
}

/// A grid of cards of different heights, in the shortest column each time.
///
/// Hand-rolled, and the reason is worth writing down: a real masonry measures
/// its children, and measuring means a layout pass per card before anything is
/// painted. A note card's height is a function of what is in the summary — a
/// title and up to four preview lines — so it can be *estimated* exactly well
/// enough to balance the columns, with no measurement and no package. The cost
/// is that columns can end up slightly uneven; the alternative costs a frame.
class _MasonryGrid extends StatelessWidget {
  const _MasonryGrid({required this.wrap, required this.children});

  /// The cards, unwrapped, because the estimate below reads what is on them.
  final List<_NoteCard> children;

  /// What actually goes in the column: the card, or the card made draggable.
  /// Kept apart from [children] so that balancing the columns stays a question
  /// about a note's contents and not about what is wrapped around it.
  final Widget Function(_NoteCard) wrap;

  /// Roughly what a card will take: the padding and title, a line per preview
  /// item, and the matched line when it is a search result. Units are arbitrary
  /// and only ever compared with each other.
  static double _estimate(_NoteCard card) =>
      64 +
      (card.note.title.isEmpty ? 0 : 28) +
      card.note.preview.length * 22 +
      (card.matched == null ? 0 : 22) +
      // One row of chips, whatever the number: they wrap, and a card with eight
      // labels on it is rarer than the error in guessing how many rows they take.
      (card.note.tags.isEmpty ? 0 : 30);

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        // About 260 logical pixels a column: narrower and a shopping list wraps
        // every line, wider and a phone shows one column when it could show two.
        final columns = (constraints.maxWidth / 260).floor().clamp(1, 4);
        final buckets = List.generate(columns, (_) => <Widget>[]);
        final heights = List.filled(columns, 0.0);
        for (final card in children) {
          var shortest = 0;
          for (var i = 1; i < columns; i++) {
            if (heights[i] < heights[shortest]) shortest = i;
          }
          buckets[shortest].add(
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: wrap(card),
            ),
          );
          heights[shortest] += _estimate(card);
        }
        return SingleChildScrollView(
          padding: const EdgeInsets.all(8),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              for (var i = 0; i < columns; i++) ...[
                if (i > 0) const SizedBox(width: 8),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: buckets[i],
                  ),
                ),
              ],
            ],
          ),
        );
      },
    );
  }
}

/// One note, as a card: what it is called, what is on it, how much is done, and
/// what this person has made of it.
class _NoteCard extends StatelessWidget {
  const _NoteCard({
    super.key,
    required this.note,
    required this.onTap,
    this.onHold,
    this.matched,
  });

  final NoteSummary note;
  final VoidCallback onTap;

  /// The card's menu, when the card is not draggable. Null when it is: there the
  /// long press is the drag, and [_DraggableCard] decides whether it was a press
  /// after all.
  final VoidCallback? onHold;

  /// The line a search matched, when this card is a result. Shown because the
  /// preview is four items and the match may be the twelfth.
  final String? matched;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final hidden = note.items - note.preview.length;
    return Card(
      margin: EdgeInsets.zero,
      color: cardColour(context, note.colour),
      child: InkWell(
        key: Key('note:${note.id}'),
        onTap: onTap,
        onLongPress: onHold,
        child: Padding(
          padding: const EdgeInsets.all(12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              if (note.title.isNotEmpty || note.pinned)
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (note.title.isNotEmpty)
                      Expanded(
                        child: Text(
                          note.title,
                          style: theme.textTheme.titleMedium,
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                        ),
                      )
                    else
                      const Spacer(),
                    if (note.pinned)
                      Icon(
                        Icons.push_pin,
                        key: const Key('pinned'),
                        size: 16,
                        color: theme.textTheme.bodySmall?.color,
                      ),
                  ],
                ),
              if (note.title.isNotEmpty && note.preview.isNotEmpty)
                const SizedBox(height: 6),
              for (final text in note.preview)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 1),
                  child: Text(
                    '• $text',
                    style: theme.textTheme.bodyMedium,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              if (hidden > 0)
                Padding(
                  padding: const EdgeInsets.only(top: 4),
                  child: Text(
                    '+$hidden more',
                    style: theme.textTheme.bodySmall,
                  ),
                ),
              if (note.title.isEmpty && note.items == 0)
                Text(
                  'Untitled list',
                  style: theme.textTheme.titleMedium?.copyWith(
                    color: theme.disabledColor,
                  ),
                ),
              if (note.items > 0) ...[
                const SizedBox(height: 8),
                Text(
                  '${note.checked} of ${note.items} done',
                  style: theme.textTheme.bodySmall,
                ),
              ],
              // The labels, as small chips. On the card and not only in the
              // drawer, because a note's filing is part of what it is — and
              // because a shared list that one person has labelled looks the
              // same to everyone else, which is the whole point of the account
              // document.
              if (note.tags.isNotEmpty) ...[
                const SizedBox(height: 8),
                Wrap(
                  spacing: 4,
                  runSpacing: 4,
                  children: [
                    for (final tag in note.tags)
                      _Chip(key: Key('chip:${note.id}:$tag'), label: tag),
                  ],
                ),
              ],
              // Only when the match is not already on the card: a hit on the
              // title, or on one of the four items the preview shows, needs no
              // second copy of itself underneath.
              if (matched != null &&
                  matched != note.title &&
                  !note.preview.contains(matched)) ...[
                const SizedBox(height: 6),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(
                      Icons.search,
                      size: 14,
                      color: theme.textTheme.bodySmall?.color,
                    ),
                    const SizedBox(width: 4),
                    Expanded(
                      child: Text(
                        matched!,
                        key: Key('matched:${note.id}'),
                        style: theme.textTheme.bodySmall,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                  ],
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// One label on a card. Small, flat and not tappable: a card's own gesture is
/// opening the note, and a chip that could be pressed would be a second target
/// inside the first.
class _Chip extends StatelessWidget {
  const _Chip({super.key, required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(10),
        // Over whatever colour the card is, so a chip reads on all ten of them.
        color: theme.colorScheme.onSurface.withValues(alpha: 0.08),
      ),
      child: Text(
        label,
        style: theme.textTheme.labelSmall,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      ),
    );
  }
}

/// The labels on one note: which of this person's labels it has, and a field for
/// one it has not.
///
/// It keeps its own copy of what is on the note, which is the one place in this
/// screen that does. Everything else here is a function of the library's stream,
/// but a dialog is above the `StreamBuilder` that would rebuild it — so it asks
/// the library after every change and redraws from the answer. That answer is the
/// account document's and not this widget's guess, which matters for the case
/// below: a name typed in a casing this device has already used comes back as
/// the casing it has.
class _LabelsDialog extends StatefulWidget {
  const _LabelsDialog({required this.library, required this.note});

  final NoteLibrary library;
  final NoteSummary note;

  @override
  State<_LabelsDialog> createState() => _LabelsDialogState();
}

class _LabelsDialogState extends State<_LabelsDialog> {
  final _field = TextEditingController();

  /// What the note has, and every label this device knows, as of the last answer.
  late Set<String> _on = widget.note.tags.toSet();
  late List<Tag> _all = widget.library.tags;

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  /// Re-read both from the library. The note's own labels come off the list
  /// rather than being tracked here, so that a label the core reused under
  /// another casing is drawn as the label that exists.
  void _reread() {
    final summary = widget.library.notes
        .where((note) => note.id == widget.note.id)
        .firstOrNull;
    setState(() {
      _all = widget.library.tags;
      _on = (summary?.tags ?? const <String>[]).toSet();
    });
  }

  Future<void> _toggle(String name, bool on) async {
    if (on) {
      await widget.library.addTag(widget.note.id, name);
    } else {
      await widget.library.removeTag(widget.note.id, name);
    }
    if (mounted) _reread();
  }

  Future<void> _add() async {
    final typed = _field.text;
    if (typed.trim().isEmpty) return;
    await widget.library.addTag(widget.note.id, typed);
    if (!mounted) return;
    _field.clear();
    _reread();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      key: const Key('labels-dialog'),
      title: const Text('Labels'),
      // `double.maxFinite` rather than a number: an `AlertDialog` measures its
      // content's intrinsic width, and the scrollable below cannot report one.
      // This is the documented way to put a list in a dialog, and it means the
      // dialog is as wide as it is allowed to be rather than as wide as the
      // longest label.
      content: SizedBox(
        width: double.maxFinite,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: TextField(
                    key: const Key('label-field'),
                    controller: _field,
                    autofocus: true,
                    // The same number the core would cut a name at: a label has to
                    // fit on a chip, and anything longer is text for the note.
                    maxLength: maxLabelLength,
                    decoration: const InputDecoration(
                      hintText: 'New label',
                      border: OutlineInputBorder(),
                      isDense: true,
                      counterText: '',
                    ),
                    onSubmitted: (_) => _add(),
                  ),
                ),
                IconButton(
                  key: const Key('label-add'),
                  icon: const Icon(Icons.add),
                  tooltip: 'Add this label',
                  onPressed: _add,
                ),
              ],
            ),
            if (_all.isNotEmpty)
              // Bounded, because a dialog cannot be taller than the screen and a
              // person with forty labels still has to reach the ones at the end.
              Flexible(
                child: ListView(
                  shrinkWrap: true,
                  children: [
                    for (final tag in _all)
                      CheckboxListTile(
                        key: Key('label-check-${tag.name}'),
                        value: _on.contains(tag.name),
                        title: Text(tag.name),
                        dense: true,
                        controlAffinity: ListTileControlAffinity.leading,
                        onChanged: (on) => _toggle(tag.name, on ?? false),
                      ),
                  ],
                ),
              ),
          ],
        ),
      ),
      actions: [
        TextButton(
          key: const Key('labels-done'),
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Done'),
        ),
      ],
    );
  }
}

/// Asks for a share blob and pops with it.
///
/// A widget of its own rather than a closure in the screen, because the text
/// controller has to outlive the dialog's own teardown and be disposed after it
/// — which is exactly what a `State` does and a local variable does not.
class _JoinDialog extends StatefulWidget {
  const _JoinDialog();

  @override
  State<_JoinDialog> createState() => _JoinDialogState();
}

class _JoinDialogState extends State<_JoinDialog> {
  final _controller = TextEditingController();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Join a list'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Text(
            'Paste the blob from the device that has the list. It is added to '
            'the lists already here.',
          ),
          const SizedBox(height: 12),
          TextField(
            key: const Key('join-blob-field'),
            controller: _controller,
            autofocus: true,
            decoration: const InputDecoration(
              hintText: 'Share blob',
              border: OutlineInputBorder(),
              isDense: true,
            ),
            onSubmitted: (value) => Navigator.of(context).pop(value.trim()),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        TextButton(
          key: const Key('join-confirm'),
          onPressed: () => Navigator.of(context).pop(_controller.text.trim()),
          child: const Text('Join'),
        ),
      ],
    );
  }
}
