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

import 'package:flutter/material.dart';

import '../core/note_library.dart';
import 'checklist_screen.dart';
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

class NoteListScreen extends StatefulWidget {
  const NoteListScreen({super.key, required this.library});

  final NoteLibrary library;

  @override
  State<NoteListScreen> createState() => _NoteListScreenState();
}

class _NoteListScreenState extends State<NoteListScreen> {
  /// What this person last chose, which the library remembers for them.
  late NoteLayout _layout = widget.library.layout;
  _Shelf _shelf = _Shelf.notes;

  /// Open a note, and hold the handle open only while its screen is on top.
  ///
  /// The library keeps the note itself open and syncing; what this disposes is
  /// the watcher behind the screen that has just gone away. Leaking one per
  /// visit would leave a Rust task per note per open, for ever.
  Future<void> _openNote(String id) async {
    final core = widget.library.openNote(id);
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (context) => ChecklistScreen(core: core),
      ),
    );
    await core.dispose();
  }

  Future<void> _createNote() async {
    final id = await widget.library.createNote();
    if (!mounted) return;
    await _openNote(id);
  }

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

  /// The actions for one note, on a sheet that comes up from the bottom.
  ///
  /// A long press rather than a button on every card: a card is mostly the
  /// note, and nine of ten visits to this screen are to read one or open one.
  Future<void> _actionsFor(NoteSummary note) async {
    await showModalBottomSheet<void>(
      context: context,
      builder: (context) => _NoteActions(note: note, library: widget.library),
    );
  }

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<List<NoteSummary>>(
      stream: widget.library.watchNotes(),
      initialData: widget.library.notes,
      builder: (context, snapshot) {
        final all = snapshot.data ?? const <NoteSummary>[];
        final notes = all.where(_shelf.holds).toList();
        return Scaffold(
          appBar: AppBar(
            title: Text(_shelf.title),
            actions: [
              SyncStatusButton(
                stream: widget.library.watchSync(),
                initial: widget.library.syncStatus,
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
              if (widget.library.canJoin && _shelf == _Shelf.notes)
                IconButton(
                  key: const Key('join-button'),
                  icon: const Icon(Icons.group_add_outlined),
                  tooltip: 'Join a list',
                  onPressed: _join,
                ),
            ],
          ),
          drawer: _Shelves(
            current: _shelf,
            counts: {
              for (final shelf in _Shelf.values)
                shelf: all.where(shelf.holds).length,
            },
            onChosen: (shelf) => setState(() => _shelf = shelf),
          ),
          // Only where a new note would land. One made while looking at the
          // trash would appear in a grid that is not on screen.
          floatingActionButton: _shelf == _Shelf.notes
              ? FloatingActionButton(
                  key: const Key('new-note-button'),
                  tooltip: 'New list',
                  onPressed: _createNote,
                  child: const Icon(Icons.add),
                )
              : null,
          body: notes.isEmpty
              ? Center(
                  key: const Key('no-notes'),
                  child: Text(_shelf.empty),
                )
              : _NoteCollection(
                  notes: notes,
                  layout: _layout,
                  onTap: _openNote,
                  onHold: _actionsFor,
                ),
        );
      },
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
  });

  final List<NoteSummary> notes;
  final NoteLayout layout;
  final void Function(String id) onTap;
  final void Function(NoteSummary note) onHold;

  @override
  Widget build(BuildContext context) {
    final cards = [
      for (final note in notes)
        _NoteCard(
          key: ValueKey(note.id),
          note: note,
          onTap: () => onTap(note.id),
          onHold: () => onHold(note),
        ),
    ];
    return layout == NoteLayout.grid
        ? _MasonryGrid(children: cards)
        : ListView(
            padding: const EdgeInsets.all(8),
            children: [
              for (final card in cards)
                Padding(
                  padding: const EdgeInsets.only(bottom: 8),
                  child: card,
                ),
            ],
          );
  }
}

/// The three shelves, with how many notes are on each.
class _Shelves extends StatelessWidget {
  const _Shelves({
    required this.current,
    required this.counts,
    required this.onChosen,
  });

  final _Shelf current;
  final Map<_Shelf, int> counts;
  final void Function(_Shelf) onChosen;

  static const _icons = {
    _Shelf.notes: Icons.lightbulb_outline,
    _Shelf.archive: Icons.archive_outlined,
    _Shelf.trash: Icons.delete_outline,
  };

  @override
  Widget build(BuildContext context) {
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
              selected: shelf == current,
              onTap: () {
                Navigator.of(context).pop();
                onChosen(shelf);
              },
            ),
        ],
      ),
    );
  }
}

/// What can be done to one note, without opening it.
class _NoteActions extends StatelessWidget {
  const _NoteActions({required this.note, required this.library});

  final NoteSummary note;
  final NoteLibrary library;

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
              key: const Key('action-archive'),
              leading: Icon(
                note.archived ? Icons.unarchive_outlined : Icons.archive_outlined,
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
  const _MasonryGrid({required this.children});

  final List<_NoteCard> children;

  /// Roughly what a card will take: the padding and title, plus a line per
  /// preview item. Units are arbitrary and only ever compared with each other.
  static double _estimate(NoteSummary note) =>
      64 + (note.title.isEmpty ? 0 : 28) + note.preview.length * 22;

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
            Padding(padding: const EdgeInsets.only(bottom: 8), child: card),
          );
          heights[shortest] += _estimate(card.note);
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
    required this.onHold,
  });

  final NoteSummary note;
  final VoidCallback onTap;
  final VoidCallback onHold;

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
            ],
          ),
        ),
      ),
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
