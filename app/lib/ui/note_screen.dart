/// One note, opened from the note list: a body of text, a checklist, or — when a
/// merge has left content on both sides — both.
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
import '../core/note_library.dart';
import '../core/note_view.dart';
import 'storage_banner.dart';
import 'sync_button.dart';

class NoteScreen extends StatefulWidget {
  const NoteScreen({super.key, required this.core, required this.storage});

  final NoteCore core;

  /// Whether the device is keeping what is typed here.
  ///
  /// The device's question rather than this note's, which is why it comes from
  /// beside the core rather than out of it — but this is the screen a person is
  /// typing into, so it is where the answer matters most. Required rather than
  /// optional: a screen that quietly did not show it would be exactly the bug
  /// this is for.
  final StorageHealth storage;

  @override
  State<NoteScreen> createState() => _NoteScreenState();
}

class _NoteScreenState extends State<NoteScreen> {
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

  _ItemRow _row(ItemView item) => _ItemRow(
    key: ValueKey(item.id),
    item: item,
    onChecked: (v) => widget.core.setChecked(item.id, v ?? false),
    onTextChanged: (t) => widget.core.setItemText(item.id, t),
    onDelete: () => widget.core.removeItem(item.id),
  );

  _BodyField _bodyField(NoteView note) => _BodyField(
    // Not keyed on the text, unlike the title: the field keeps its own
    // controller across remote edits so the caret stays where the person left
    // it. See [_BodyFieldState.didUpdateWidget].
    key: const Key('body-field'),
    body: note.body,
    // A checklist only has a body when a merge left one there, so it is
    // prompted for as what it is rather than as the place to start typing.
    hint: note.kind == NoteKind.text ? 'Note' : 'Text',
    onEdit: widget.core.editBody,
  );

  /// A text note: the body, and under it any items a merge left on it — drawn,
  /// because the rule is that a merge never hides anything, but not reorderable,
  /// because switching to a checklist is the way to work on them as a list.
  Widget _text(NoteView note) => ListView(
    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
    children: [_bodyField(note), for (final item in note.items) _row(item)],
  );

  /// A checklist, with any body a merge left on it above the items.
  Widget _checklist(NoteView note) => Column(
    children: [
      Expanded(
        child: note.items.isEmpty && note.body.isEmpty
            ? const Center(
                key: Key('empty-state'),
                child: Text('Nothing on this list yet.'),
              )
            : ReorderableListView.builder(
                header: note.body.isEmpty
                    ? null
                    : Padding(
                        padding: const EdgeInsets.symmetric(horizontal: 16),
                        child: _bodyField(note),
                      ),
                itemCount: note.items.length,
                // `onReorderItem`, not the deprecated `onReorder`: it reports
                // the target index with the dragged item already taken out of
                // the list, which is exactly what the core means by "move to
                // index N". The old callback counted the dragged item too and
                // needed adjusting here, which is a subtle off-by-one to keep
                // owning.
                onReorderItem: (oldIndex, newIndex) {
                  widget.core.moveItem(note.items[oldIndex].id, newIndex);
                },
                itemBuilder: (context, index) => _row(note.items[index]),
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
  );

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<NoteView>(
      stream: widget.core.watch(),
      initialData: widget.core.current,
      builder: (context, snapshot) {
        final note = snapshot.data ?? const NoteView.empty();
        final isText = note.kind == NoteKind.text;
        return Scaffold(
          appBar: AppBar(
            title: _TitleField(
              // Keyed on the title so that a title arriving from another device
              // rebuilds the field instead of leaving stale text in it.
              key: ValueKey('title:${note.title}'),
              title: note.title,
              hint: isText ? 'Title' : 'Untitled list',
              onChanged: widget.core.setTitle,
            ),
            actions: [
              // Keep's "show checkboxes": the same note, its content carried
              // across. Shared, so it switches the note for everyone on it.
              IconButton(
                key: const Key('kind-toggle'),
                icon: Icon(isText ? Icons.check_box_outlined : Icons.notes),
                tooltip: isText ? 'Show checkboxes' : 'Hide checkboxes',
                onPressed: () => widget.core.setKind(
                  isText ? NoteKind.checklist : NoteKind.text,
                ),
              ),
              SyncStatusButton(
                stream: widget.core.watchSync(),
                initial: widget.core.syncStatus,
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
              StorageBanner(storage: widget.storage),
              Expanded(child: isText ? _text(note) : _checklist(note)),
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
  const _TitleField({
    super.key,
    required this.title,
    required this.hint,
    required this.onChanged,
  });

  final String title;
  final String hint;
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
      decoration: InputDecoration(
        hintText: widget.hint,
        border: InputBorder.none,
      ),
      onSubmitted: (_) => _commit(),
    );
  }
}

/// A note's body, sent as it is typed rather than on losing focus as the title
/// is.
///
/// Per change because the body is where two people type into one note at once,
/// and a body held back until someone taps away would merge as one large edit
/// instead of as the keystrokes it was — which is the difference between the
/// backend's text type interleaving two people's words and it stacking two
/// paragraphs. Each send carries the whole text, and the core turns it into the
/// splice that was typed.
///
/// Each send says what the text was *before* as well as after, because the core
/// may have moved on since this field last drew — a peer's words arriving
/// between two keystrokes — and a send of the new text alone would type over
/// them. With both, the core replays this person's change onto whatever the
/// body now says ([NoteCore.editBody]).
///
/// Three more things make it safe, and they live here because only this widget
/// knows when the person is mid-word:
///
/// * **Sends go one at a time.** The bridge runs calls on a pool, so two sends in
///   flight could land in either order. While one is in flight the latest text
///   waits, and whatever it says when the first returns is what goes next.
/// * **This device's own text coming back is not news.** Views arrive late, and
///   one carrying text this field sent a moment ago would, if adopted, wind the
///   field back to it. So what was sent is remembered until it comes back, and
///   a view carrying any of it is passed over; while a send is pending, every
///   view is.
/// * **A remote change keeps the caret.** Replacing the controller's text would
///   put the cursor at the end, mid-sentence, every time the other person typed.
///   The change is found as the one splice between the old text and the new,
///   and the selection is carried across it.
class _BodyField extends StatefulWidget {
  const _BodyField({
    super.key,
    required this.body,
    required this.hint,
    required this.onEdit,
  });

  final String body;
  final String hint;
  final Future<void> Function(String from, String to) onEdit;

  @override
  State<_BodyField> createState() => _BodyFieldState();
}

class _BodyFieldState extends State<_BodyField> {
  late final _controller = TextEditingController(text: widget.body);

  /// The text the core was last told this field holds, or last told this field
  /// it holds: what the next send changes *from*.
  late String _base = widget.body;

  /// Texts sent whose view has not come back yet, oldest first.
  final List<String> _sent = [];
  bool _sending = false;
  String? _queued;

  Future<void> _send(String text) async {
    if (_sending) {
      _queued = text;
      return;
    }
    _sending = true;
    try {
      String? next = text;
      while (next != null) {
        _queued = null;
        _sent.add(next);
        final from = _base;
        _base = next;
        await widget.onEdit(from, next);
        next = _queued;
      }
    } finally {
      _sending = false;
    }
    // A view that came in while the queue was busy was passed over; look at
    // the latest now, in case it was someone else's.
    if (mounted) _consider(widget.body);
  }

  @override
  void didUpdateWidget(_BodyField old) {
    super.didUpdateWidget(old);
    _consider(widget.body);
  }

  /// A body arrived. Take it unless it is this field's own text coming back.
  void _consider(String body) {
    if (_sending) return;
    final own = _sent.lastIndexOf(body);
    if (own >= 0) {
      // Everything sent up to it has come back too, by now or never.
      _sent.removeRange(0, own + 1);
      return;
    }
    if (body == _controller.text) return;
    // Someone else's change, or theirs merged with this field's: either way
    // it is what the core now holds, and what the next send changes from.
    _sent.clear();
    _base = body;
    _adopt(body);
  }

  /// Show [body], keeping the selection where it was relative to the text
  /// around it.
  void _adopt(String body) {
    final old = _controller.text;
    if (body == old) return;
    final at = _prefix(old, body);
    final tail = _suffix(old, body, at);
    final oldEnd = old.length - tail;
    final newEnd = body.length - tail;
    int carry(int offset) {
      if (offset <= at) return offset;
      if (offset >= oldEnd) return offset - oldEnd + newEnd;
      return newEnd;
    }

    final selection = _controller.selection;
    _controller.value = TextEditingValue(
      text: body,
      selection: selection.isValid
          ? TextSelection(
              baseOffset: carry(selection.baseOffset),
              extentOffset: carry(selection.extentOffset),
            )
          : TextSelection.collapsed(offset: body.length),
    );
  }

  static int _prefix(String a, String b) {
    var i = 0;
    while (i < a.length && i < b.length && a.codeUnitAt(i) == b.codeUnitAt(i)) {
      i++;
    }
    return i;
  }

  static int _suffix(String a, String b, int prefix) {
    var i = 0;
    while (i < a.length - prefix &&
        i < b.length - prefix &&
        a.codeUnitAt(a.length - 1 - i) == b.codeUnitAt(b.length - 1 - i)) {
      i++;
    }
    return i;
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return TextField(
      key: const Key('body-text'),
      controller: _controller,
      keyboardType: TextInputType.multiline,
      maxLines: null,
      decoration: InputDecoration(
        hintText: widget.hint,
        border: InputBorder.none,
      ),
      onChanged: _send,
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
