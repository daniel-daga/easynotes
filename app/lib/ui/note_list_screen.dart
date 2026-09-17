/// The screen the app opens on: every note this device holds.
///
/// It obeys the same rule as the checklist. There is one `StreamBuilder` over
/// the library's note list and everything below it is a function of what that
/// yields — a note created here, a note joined, and a note someone else edited
/// on another device all reach the grid by the same path, because the core
/// refreshes the list for all three.
///
/// The layout is Keep's: a masonry grid of cards, with a toggle to a single
/// column. Which of the two you last chose is a view setting, and view settings
/// are personal data that belongs in the account document — item 6 of the Slice
/// 2 plan — so for now it lasts as long as the screen does.
library;

import 'package:flutter/material.dart';

import '../core/note_library.dart';
import 'checklist_screen.dart';
import 'sync_button.dart';

class NoteListScreen extends StatefulWidget {
  const NoteListScreen({super.key, required this.library});

  final NoteLibrary library;

  @override
  State<NoteListScreen> createState() => _NoteListScreenState();
}

class _NoteListScreenState extends State<NoteListScreen> {
  bool _grid = true;

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

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('EasyNotes'),
        actions: [
          SyncStatusButton(
            stream: widget.library.watchSync(),
            initial: widget.library.syncStatus,
          ),
          IconButton(
            key: const Key('layout-toggle'),
            icon: Icon(_grid ? Icons.view_agenda_outlined : Icons.grid_view),
            tooltip: _grid ? 'Show as a list' : 'Show as a grid',
            onPressed: () => setState(() => _grid = !_grid),
          ),
          if (widget.library.canJoin)
            IconButton(
              key: const Key('join-button'),
              icon: const Icon(Icons.group_add_outlined),
              tooltip: 'Join a list',
              onPressed: _join,
            ),
        ],
      ),
      floatingActionButton: FloatingActionButton(
        key: const Key('new-note-button'),
        tooltip: 'New list',
        onPressed: _createNote,
        child: const Icon(Icons.add),
      ),
      body: StreamBuilder<List<NoteSummary>>(
        stream: widget.library.watchNotes(),
        initialData: widget.library.notes,
        builder: (context, snapshot) {
          final notes = snapshot.data ?? const <NoteSummary>[];
          if (notes.isEmpty) {
            return const Center(
              key: Key('no-notes'),
              child: Text('No lists yet. Make one.'),
            );
          }
          final cards = [
            for (final note in notes)
              _NoteCard(
                key: ValueKey(note.id),
                note: note,
                onTap: () => _openNote(note.id),
              ),
          ];
          return _grid
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
        },
      ),
    );
  }
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

/// One note, as a card: what it is called, what is on it, how much is done.
class _NoteCard extends StatelessWidget {
  const _NoteCard({super.key, required this.note, required this.onTap});

  final NoteSummary note;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final hidden = note.items - note.preview.length;
    return Card(
      margin: EdgeInsets.zero,
      child: InkWell(
        key: Key('note:${note.id}'),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              if (note.title.isNotEmpty)
                Text(
                  note.title,
                  style: theme.textTheme.titleMedium,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
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
