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
import '../core/sync_status.dart';

class ChecklistScreen extends StatefulWidget {
  const ChecklistScreen({super.key, required this.core, this.onJoin});

  final NoteCore core;

  /// Called with a share blob the user pasted. Joining replaces the note this
  /// device holds, which only the layer that owns the core can do — so it is a
  /// callback rather than something this screen performs. Null where joining is
  /// not possible yet, and the button is then not offered.
  final Future<void> Function(String blob)? onJoin;

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

  Future<void> _showJoinDialog() async {
    final blob = await showDialog<String>(
      context: context,
      builder: (context) => const _JoinDialog(),
    );
    if (blob == null || blob.isEmpty) return;
    await widget.onJoin?.call(blob);
  }

  Future<void> _showSyncDetail(SyncStatus status) async {
    await showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Sync'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(_syncSentence(status)),
            if (status.lastError != null) ...[
              const SizedBox(height: 12),
              SelectableText(
                status.lastError!,
                key: const Key('sync-error'),
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
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
              // Its own stream: sync state changes on the relay's schedule, not
              // on the note's, and rebuilding the list every time a socket
              // reconnects would be silly.
              StreamBuilder<SyncStatus>(
                stream: widget.core.watchSync(),
                initialData: widget.core.syncStatus,
                builder: (context, snapshot) {
                  final status = snapshot.data ?? const SyncStatus.offline();
                  return _SyncButton(
                    status: status,
                    onPressed: () => _showSyncDetail(status),
                  );
                },
              ),
              if (widget.onJoin != null)
                IconButton(
                  key: const Key('join-button'),
                  icon: const Icon(Icons.group_add_outlined),
                  tooltip: 'Join a list',
                  onPressed: _showJoinDialog,
                ),
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

/// Asks for a share blob and pops with it.
///
/// A widget of its own rather than a closure in [ChecklistScreen], because the
/// text controller has to outlive the dialog's own teardown and be disposed
/// after it — which is exactly what a `State` does and a local variable does not.
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
            'Paste the blob from the device that has the list. This device '
            'then holds that list instead of its own.',
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
          onPressed: () =>
              Navigator.of(context).pop(_controller.text.trim()),
          child: const Text('Join'),
        ),
      ],
    );
  }
}

/// What the sync state says, in one sentence, for a tooltip or a dialog.
///
/// Deliberately about the user's data rather than about the socket: "waiting to
/// reach the relay" is a fact they can act on, `ECONNREFUSED` is not. The error
/// itself is still there for anyone who opens the detail.
String _syncSentence(SyncStatus status) {
  final waiting = switch (status.pending) {
    0 => '',
    1 => ', 1 edit waiting',
    _ => ', ${status.pending} edits waiting',
  };
  return switch (status.state) {
    SyncState.offline => 'Not syncing$waiting',
    SyncState.connecting => 'Connecting$waiting',
    SyncState.online => 'Catching up$waiting',
    SyncState.synced =>
      status.pending == 0 ? 'Up to date' : 'Sending$waiting',
  };
}

/// The sync state, as one button that explains itself when tapped.
class _SyncButton extends StatelessWidget {
  const _SyncButton({required this.status, required this.onPressed});

  final SyncStatus status;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final icon = switch (status.state) {
      SyncState.offline => Icons.cloud_off_outlined,
      SyncState.connecting => Icons.cloud_queue_outlined,
      SyncState.online => Icons.cloud_sync_outlined,
      SyncState.synced => Icons.cloud_done_outlined,
    };
    final button = IconButton(
      key: const Key('sync-status'),
      icon: Icon(icon),
      tooltip: _syncSentence(status),
      onPressed: onPressed,
    );
    if (status.pending == 0) return button;
    return Badge.count(count: status.pending, child: button);
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
