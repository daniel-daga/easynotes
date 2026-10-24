import 'package:flutter/material.dart';

import '../core/note_library.dart';

/// Manage the account's labels, or choose existing labels for some notes.
///
/// With [noteId] or [noteIds], label definitions are read-only: only note
/// membership changes. For several notes a label is ticked when every one has
/// it, dashed when some do, and ticking it puts it on all of them.
class LabelManagerScreen extends StatefulWidget {
  const LabelManagerScreen({
    super.key,
    required this.library,
    this.noteId,
    this.noteIds,
  });

  final NoteLibrary library;
  final String? noteId;
  final Set<String>? noteIds;

  @override
  State<LabelManagerScreen> createState() => _LabelManagerScreenState();
}

class _LabelManagerScreenState extends State<LabelManagerScreen> {
  final _field = TextEditingController();
  late final _notes = widget.library.watchNotes();
  bool _busy = false;

  /// The notes whose labels are being chosen; empty when managing labels.
  Set<String> get _ids =>
      widget.noteIds ?? {?widget.noteId};

  bool get _selecting => _ids.isNotEmpty;

  /// One note goes through the one-note calls, several through the calls that
  /// change them all at once.
  Future<void> _add(List<String> ids, String name) async {
    if (ids.length == 1) {
      await widget.library.addTag(ids.single, name);
    } else {
      await widget.library.addTagMany(ids, name);
    }
  }

  Future<void> _remove(List<String> ids, String label) async {
    if (ids.length == 1) {
      await widget.library.removeTag(ids.single, label);
    } else {
      await widget.library.removeTagMany(ids, label);
    }
  }

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  Future<void> _run(Future<void> Function() action) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await action();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Could not update labels: $error')),
        );
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _create() async {
    final name = _field.text.trim();
    if (name.isEmpty || _selecting) return;
    await _run(() async {
      await widget.library.makeTag(name);
      if (mounted) _field.clear();
    });
  }

  Future<void> _addToNote(String name) async {
    final ids = _present();
    if (ids.isEmpty || name.trim().isEmpty) return;
    await _run(() => _add(ids, name));
  }

  /// The chosen notes this device still has, in the library's order.
  List<String> _present() => [
    for (final note in widget.library.notes)
      if (_ids.contains(note.id)) note.id,
  ];

  Future<void> _edit(Tag tag) async {
    var deleting = false;
    final name = await showDialog<String>(
      context: context,
      builder: (context) => _RenameLabelDialog(
        tag: tag,
        onDelete: () {
          deleting = true;
          Navigator.of(context).pop();
        },
      ),
    );
    if (!mounted) return;
    if (deleting) {
      await _forget(tag);
    } else if (name != null) {
      await _run(() async {
        final survivor = await widget.library.renameTag(tag.id, name);
        if (mounted && survivor != null && survivor != tag.id) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(content: Text('Merged "${tag.name}" into "$name".')),
          );
        }
      });
    }
  }

  Future<void> _forget(Tag tag) async {
    final sure = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Delete "${tag.name}"?'),
        content: Text(
          tag.notes == 0
              ? 'Nothing is labelled "${tag.name}", so nothing else changes.'
              : 'It comes off ${tag.notes} ${tag.notes == 1 ? 'note' : 'notes'}. '
                    'The notes themselves are kept.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            key: const Key('confirm-forget-label'),
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (sure == true && mounted) {
      await _run(() => widget.library.forgetTag(tag.id));
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    key: const Key('label-manager'),
    appBar: AppBar(
      title: Text(_selecting ? 'Labels' : 'Edit labels'),
      actions: [
        if (_selecting)
          TextButton(
            key: const Key('labels-done'),
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('Done'),
          ),
      ],
    ),
    body: SafeArea(
      top: false,
      child: StreamBuilder<List<NoteSummary>>(
        stream: _notes,
        initialData: widget.library.notes,
        builder: (context, snapshot) {
          final tags = widget.library.tags;
          final notes = [
            for (final note in widget.library.notes)
              if (_ids.contains(note.id)) note,
          ];
          final ids = [for (final note in notes) note.id];
          // How many of the chosen notes carry each label.
          final carrying = <String, int>{};
          for (final note in notes) {
            for (final id in note.tagIds) {
              carrying[id] = (carrying[id] ?? 0) + 1;
            }
          }
          bool? ticked(String label) => switch (carrying[label] ?? 0) {
            0 => false,
            final count when count == notes.length => true,
            _ => null,
          };
          final query = _field.text.trim().replaceAll(RegExp(r'\s+'), ' ');
          final exactMatch =
              query.isNotEmpty &&
              tags.any((tag) => tag.name.toLowerCase() == query.toLowerCase());
          final visibleTags = _selecting && query.isNotEmpty
              ? tags
                    .where(
                      (tag) =>
                          tag.name.toLowerCase().contains(query.toLowerCase()),
                    )
                    .toList()
              : tags;
          return ListView(
            padding: const EdgeInsets.symmetric(vertical: 16),
            children: [
              if (_selecting)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
                  child: TextField(
                    key: const Key('label-search-field'),
                    controller: _field,
                    enabled: !_busy,
                    maxLength: maxLabelLength,
                    decoration: InputDecoration(
                      hintText: 'Search labels',
                      prefixIcon: const Icon(Icons.search),
                      border: const OutlineInputBorder(),
                      counterText: '',
                      suffixIcon: query.isEmpty
                          ? null
                          : IconButton(
                              tooltip: 'Clear search',
                              icon: const Icon(Icons.clear),
                              onPressed: () {
                                _field.clear();
                                setState(() {});
                              },
                            ),
                    ),
                    onChanged: (_) => setState(() {}),
                    onSubmitted: (_) {
                      if (!exactMatch) _addToNote(query);
                    },
                  ),
                )
              else
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 16),
                  child: Row(
                    children: [
                      const Padding(
                        padding: EdgeInsets.only(right: 24),
                        child: Icon(Icons.add),
                      ),
                      Expanded(
                        child: TextField(
                          key: const Key('label-field'),
                          controller: _field,
                          enabled: !_busy,
                          maxLength: maxLabelLength,
                          decoration: const InputDecoration(
                            hintText: 'Create new label',
                            border: InputBorder.none,
                            counterText: '',
                          ),
                          onSubmitted: (_) => _create(),
                        ),
                      ),
                      IconButton(
                        key: const Key('label-add'),
                        tooltip: 'Create label',
                        icon: const Icon(Icons.check),
                        onPressed: _busy ? null : _create,
                      ),
                    ],
                  ),
                ),
              if (_selecting && query.isNotEmpty && !exactMatch)
                ListTile(
                  key: const Key('label-add-to-note'),
                  leading: const Icon(Icons.add),
                  title: Text('Add label "$query"'),
                  onTap: _busy || notes.isEmpty
                      ? null
                      : () => _addToNote(query),
                ),
              if (tags.isEmpty || (visibleTags.isEmpty && query.isNotEmpty))
                Padding(
                  padding: const EdgeInsets.all(24),
                  child: Text(
                    tags.isNotEmpty ? 'No matching labels.' : 'No labels yet.',
                  ),
                ),
              for (final tag in visibleTags)
                if (_selecting)
                  CheckboxListTile(
                    key: Key('label-check-${tag.name}'),
                    secondary: const Icon(Icons.label_outline),
                    title: Text(tag.name),
                    // Dashed when only some of the notes have it. Tapping a
                    // dash puts the label on the rest, as in Keep: the first
                    // tap finishes what is half done.
                    tristate: ticked(tag.id) == null,
                    value: ticked(tag.id),
                    onChanged: _busy || notes.isEmpty
                        ? null
                        : (_) => _run(
                            () => ticked(tag.id) == true
                                ? _remove(ids, tag.id)
                                : _add(ids, tag.name),
                          ),
                  )
                else
                  ListTile(
                    key: ValueKey('manage-label-${tag.id}'),
                    minVerticalPadding: 16,
                    leading: const Icon(Icons.label_outline),
                    title: Text(tag.name),
                    trailing: IconButton(
                      key: Key('label-edit-${tag.name}'),
                      tooltip: 'Edit ${tag.name}',
                      icon: const Icon(Icons.edit_outlined),
                      onPressed: _busy ? null : () => _edit(tag),
                    ),
                    onTap: _busy ? null : () => _edit(tag),
                  ),
            ],
          );
        },
      ),
    ),
  );
}

/// Ask for a label's new name.
///
/// Seeded with the name it has, because a rename is usually a correction rather
/// than a replacement, and selected so that typing over it still works.
class _RenameLabelDialog extends StatefulWidget {
  const _RenameLabelDialog({required this.tag, required this.onDelete});

  final Tag tag;
  final VoidCallback onDelete;

  @override
  State<_RenameLabelDialog> createState() => _RenameLabelDialogState();
}

class _RenameLabelDialogState extends State<_RenameLabelDialog> {
  late final _field = TextEditingController(text: widget.tag.name)
    ..selection = TextSelection(
      baseOffset: 0,
      extentOffset: widget.tag.name.length,
    );

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  void _submit() {
    final typed = _field.text.trim();
    if (typed.isEmpty) return;
    Navigator.of(context).pop(typed);
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      key: const Key('rename-label-dialog'),
      title: Text('Rename "${widget.tag.name}"'),
      content: TextField(
        key: const Key('rename-label-field'),
        controller: _field,
        autofocus: true,
        maxLength: maxLabelLength,
        decoration: const InputDecoration(
          border: OutlineInputBorder(),
          isDense: true,
          counterText: '',
        ),
        onSubmitted: (_) => _submit(),
      ),
      actions: [
        TextButton(
          key: const Key('label-forget'),
          onPressed: widget.onDelete,
          child: const Text('Delete label'),
        ),
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('rename-label-confirm'),
          onPressed: _submit,
          child: const Text('Rename'),
        ),
      ],
    );
  }
}
