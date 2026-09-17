/// The one screen Slice 1 has: a checklist.
///
/// Keep-ish enough to be recognisable; ugly is fine. What is *not* negotiable is
/// where the state comes from. There is exactly one `StreamBuilder` at the top,
/// and every widget below it is a function of the [NoteView] it yields. No
/// widget here holds a copy of the list, and no gesture edits one — a tap sends
/// an edit to the core and the screen waits to be told what happened.
///
/// That matters beyond tidiness: it is why a remote edit arriving from the relay
/// takes exactly the same path through the UI as a local one, and therefore why
/// the two-device case cannot drift from the one-device case.
library;

import 'package:flutter/material.dart';

import '../core/note_core.dart';
import '../core/note_view.dart';

class ChecklistScreen extends StatefulWidget {
  const ChecklistScreen({super.key, required this.core});

  final NoteCore core;

  @override
  State<ChecklistScreen> createState() => _ChecklistScreenState();
}

class _ChecklistScreenState extends State<ChecklistScreen> {
  final _newItemController = TextEditingController();
  final _newItemFocus = FocusNode();

  @override
  void dispose() {
    _newItemController.dispose();
    _newItemFocus.dispose();
    super.dispose();
  }

  Future<void> _addItem() async {
    final text = _newItemController.text.trim();
    if (text.isEmpty) return;
    // Clear first so a slow core cannot swallow the next thing typed.
    _newItemController.clear();
    await widget.core.addItem(text);
    _newItemFocus.requestFocus();
  }

  Future<void> _showShareBlob() async {
    final blob = await widget.core.exportShareBlob();
    if (!mounted) return;
    await showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Share this list'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              'Paste this on the other device. It carries the note key, so treat '
              'it like a password.',
            ),
            const SizedBox(height: 12),
            SelectableText(blob, key: const Key('share-blob')),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('Done'),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<NoteView>(
      stream: widget.core.watch(),
      initialData: widget.core.current,
      builder: (context, snapshot) {
        final note = snapshot.data ?? const NoteView.empty();
        return Scaffold(
          appBar: AppBar(
            title: _TitleField(
              // Keyed on the title so that a title arriving from another device
              // rebuilds the field instead of leaving stale text in it.
              key: ValueKey('title:${note.title}'),
              title: note.title,
              onChanged: widget.core.setTitle,
            ),
            actions: [
              IconButton(
                key: const Key('share-button'),
                icon: const Icon(Icons.ios_share),
                tooltip: 'Share this list',
                onPressed: _showShareBlob,
              ),
            ],
          ),
          body: Column(
            children: [
              Expanded(
                child: note.items.isEmpty
                    ? const Center(
                        key: Key('empty-state'),
                        child: Text('Nothing on this list yet.'),
                      )
                    : ReorderableListView.builder(
                        itemCount: note.items.length,
                        // `onReorderItem`, not the deprecated `onReorder`:
                        // it reports the target index with the dragged item
                        // already taken out of the list, which is exactly what
                        // the core means by "move to index N". The old callback
                        // counted the dragged item too and needed adjusting
                        // here, which is a subtle off-by-one to keep owning.
                        onReorderItem: (oldIndex, newIndex) {
                          widget.core.moveItem(
                            note.items[oldIndex].id,
                            newIndex,
                          );
                        },
                        itemBuilder: (context, index) {
                          final item = note.items[index];
                          return _ItemRow(
                            key: ValueKey(item.id),
                            item: item,
                            onChecked: (v) =>
                                widget.core.setChecked(item.id, v ?? false),
                            onTextChanged: (t) =>
                                widget.core.setItemText(item.id, t),
                            onDelete: () => widget.core.removeItem(item.id),
                          );
                        },
                      ),
              ),
              const Divider(height: 1),
              Padding(
                padding: const EdgeInsets.all(12),
                child: Row(
                  children: [
                    Expanded(
                      child: TextField(
                        key: const Key('new-item-field'),
                        controller: _newItemController,
                        focusNode: _newItemFocus,
                        decoration: const InputDecoration(
                          hintText: 'Add an item',
                          border: OutlineInputBorder(),
                          isDense: true,
                        ),
                        onSubmitted: (_) => _addItem(),
                      ),
                    ),
                    const SizedBox(width: 8),
                    IconButton.filled(
                      key: const Key('add-item-button'),
                      icon: const Icon(Icons.add),
                      tooltip: 'Add',
                      onPressed: _addItem,
                    ),
                  ],
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

/// The note title. Commits on submit or on losing focus rather than per
/// keystroke, so typing a title does not put one update per character on the
/// relay.
class _TitleField extends StatefulWidget {
  const _TitleField({super.key, required this.title, required this.onChanged});

  final String title;
  final ValueChanged<String> onChanged;

  @override
  State<_TitleField> createState() => _TitleFieldState();
}

class _TitleFieldState extends State<_TitleField> {
  late final _controller = TextEditingController(text: widget.title);
  late final _focus = FocusNode()..addListener(_onFocusChange);

  void _onFocusChange() {
    if (!_focus.hasFocus) _commit();
  }

  void _commit() {
    if (_controller.text != widget.title) widget.onChanged(_controller.text);
  }

  @override
  void dispose() {
    _focus.removeListener(_onFocusChange);
    _focus.dispose();
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return TextField(
      key: const Key('title-field'),
      controller: _controller,
      focusNode: _focus,
      style: Theme.of(context).textTheme.titleLarge,
      decoration: const InputDecoration(
        hintText: 'Untitled list',
        border: InputBorder.none,
      ),
      onSubmitted: (_) => _commit(),
    );
  }
}

/// One checklist row: checkbox, editable text, drag handle, delete.
class _ItemRow extends StatefulWidget {
  const _ItemRow({
    super.key,
    required this.item,
    required this.onChecked,
    required this.onTextChanged,
    required this.onDelete,
  });

  final ItemView item;
  final ValueChanged<bool?> onChecked;
  final ValueChanged<String> onTextChanged;
  final VoidCallback onDelete;

  @override
  State<_ItemRow> createState() => _ItemRowState();
}

class _ItemRowState extends State<_ItemRow> {
  late final _controller = TextEditingController(text: widget.item.text);
  late final _focus = FocusNode()..addListener(_onFocusChange);

  void _onFocusChange() {
    if (!_focus.hasFocus) _commit();
  }

  void _commit() {
    if (_controller.text != widget.item.text) {
      widget.onTextChanged(_controller.text);
    }
  }

  @override
  void didUpdateWidget(_ItemRow old) {
    super.didUpdateWidget(old);
    // The text changed underneath us — someone else edited this item. Take the
    // new value, unless this device is the one typing into it.
    if (widget.item.text != old.item.text && !_focus.hasFocus) {
      _controller.text = widget.item.text;
    }
  }

  @override
  void dispose() {
    _focus.removeListener(_onFocusChange);
    _focus.dispose();
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final item = widget.item;
    return ListTile(
      leading: Checkbox(
        key: Key('check:${item.id}'),
        value: item.checked,
        onChanged: widget.onChecked,
      ),
      title: TextField(
        key: Key('text:${item.id}'),
        controller: _controller,
        focusNode: _focus,
        decoration: const InputDecoration(
          border: InputBorder.none,
          isDense: true,
        ),
        style: item.checked
            ? TextStyle(
                decoration: TextDecoration.lineThrough,
                color: Theme.of(context).disabledColor,
              )
            : null,
        onSubmitted: (_) => _commit(),
      ),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          IconButton(
            key: Key('delete:${item.id}'),
            icon: const Icon(Icons.close),
            tooltip: 'Delete',
            onPressed: widget.onDelete,
          ),
          const Icon(Icons.drag_handle),
        ],
      ),
    );
  }
}
