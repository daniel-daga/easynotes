/// One note, opened from the note list: a body of text, a checklist, or — when a
/// merge has left content on both sides — both.
///
/// Keep-ish enough to be recognisable; ugly is fine. What is *not* negotiable is
/// where the state comes from. The note content comes from [NoteView], and its
/// personal appearance and placement come from the library's note list. No
/// widget here holds a copy of either, and no gesture edits a local snapshot —
/// each action goes to the core or library and the screen waits for its stream.
///
/// That matters beyond tidiness: it is why a remote edit arriving from the relay
/// takes exactly the same path through the UI as a local one, and therefore why
/// the two-device case cannot drift from the one-device case.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/content_interchange.dart';
import '../core/content_sharing.dart';
import '../core/note_core.dart';
import '../core/note_library.dart';
import '../core/note_view.dart';
import 'invites.dart';
import 'note_appearance.dart';
import 'storage_banner.dart';

class NoteScreen extends StatefulWidget {
  const NoteScreen({
    super.key,
    required this.core,
    required this.storage,
    required this.noteId,
    required this.invites,
    this.library,
    this.onLabels,
    this.onReminders,
  });

  final NoteCore core;

  /// Which note this is, for the invite made from here.
  final String noteId;

  /// How this note is shared with another person. The device's, like
  /// [storage], because an invite lives on the relay the device talks to.
  final Invites invites;

  /// The note's personal placement and appearance controls. Optional for small
  /// isolated screen previews; the app supplies it when opening a note.
  final NoteLibrary? library;
  final Future<void> Function()? onLabels;
  final Future<void> Function()? onReminders;

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
  final Map<String, GlobalKey<_ItemRowState>> _itemRowKeys = {};
  final Map<String, FocusNode> _itemFocusNodes = {};
  final Set<String> _newBlankItems = {};
  int _pendingItemWrites = 0;
  Completer<void>? _itemWritesDone;
  bool _allowPop = false;
  bool _waitingToPop = false;

  /// Read once from the person's setting, then written back on every tap, so
  /// the next note (on any of their devices) opens the way this one was left.
  late bool _crossedOffExpanded = widget.library?.tickedShown ?? false;
  bool _deletionPopScheduled = false;
  late final Stream<List<NoteSummary>>? _noteSummaries = widget.library
      ?.watchNotes();

  @override
  void dispose() {
    for (final focus in _itemFocusNodes.values) {
      focus.dispose();
    }
    super.dispose();
  }

  void _toggleCrossedOff() {
    setState(() => _crossedOffExpanded = !_crossedOffExpanded);
    // A setting that failed to save costs nothing but the next note opening
    // folded; not worth interrupting someone ticking off their shopping.
    unawaited(
      widget.library?.setTickedShown(_crossedOffExpanded).catchError((_) {}),
    );
  }

  FocusNode _focusForItem(String id) =>
      _itemFocusNodes.putIfAbsent(id, FocusNode.new);

  void _itemWriteStateChanged(bool writing) {
    if (writing) {
      if (_pendingItemWrites == 0) _itemWritesDone = Completer<void>();
      _pendingItemWrites++;
    } else {
      _pendingItemWrites--;
      if (_pendingItemWrites == 0) _itemWritesDone?.complete();
    }
    if (mounted) setState(() {});
  }

  Future<void> _popAfterItemWrites() async {
    if (_waitingToPop) return;
    _waitingToPop = true;
    while (_pendingItemWrites > 0) {
      await _itemWritesDone?.future;
    }
    if (!mounted) return;
    for (final id in _newBlankItems.toList()) {
      final item = widget.core.current.items
          .where((item) => item.id == id)
          .firstOrNull;
      if (item != null && item.text.trim().isEmpty) {
        await widget.core.removeItem(id);
        _itemRowKeys.remove(id);
      }
    }
    _newBlankItems.clear();
    setState(() => _allowPop = true);
    await WidgetsBinding.instance.endOfFrame;
    if (mounted) Navigator.of(context).pop();
  }

  Future<void> _addListEntry() async {
    final id = await widget.core.addItem('');
    _newBlankItems.add(id);
    final items = widget.core.current.items;
    final itemIndex = items.indexWhere((item) => item.id == id);
    if (itemIndex < 0) return;

    // Keep a new unchecked entry at the end of the unchecked group, above the
    // crossed-off section, even when checked rows were originally interleaved.
    final firstChecked = items.indexWhere((item) => item.checked);
    final target = firstChecked < 0 ? items.length - 1 : firstChecked;
    if (itemIndex != target) await widget.core.moveItem(id, target);
    _focusForItem(id).requestFocus();
  }

  /// Share: the first time, set up an account and a name, then straight on
  /// to the people on this note and the link that adds one more.
  Future<void> _share() async {
    if (!await ensureSharingSetUp(context, widget.invites) || !mounted) return;
    await showDialog<void>(
      context: context,
      builder: (context) =>
          ShareDialog(invites: widget.invites, noteId: widget.noteId),
    );
  }

  Future<void> _shareContent(NoteView note) => showDialog<void>(
    context: context,
    builder: (context) => _ContentShareDialog(
      text: contentForSharing(note),
      format: note.kind == NoteKind.text ? 'text' : 'Markdown',
    ),
  );

  _ItemRow _row(
    ItemView item, {
    int? reorderIndex,
    Future<void> Function()? onSubmitted,
  }) => _ItemRow(
    key: _itemRowKeys.putIfAbsent(
      item.id,
      () => GlobalKey<_ItemRowState>(debugLabel: 'item:${item.id}'),
    ),
    item: item,
    focusNode: _focusForItem(item.id),
    reorderIndex: reorderIndex,
    onChecked: (v) => widget.core.setChecked(item.id, v ?? false),
    onTextChanged: (t) => widget.core.setItemText(item.id, t),
    onWriteStateChanged: _itemWriteStateChanged,
    onDelete: () => _removeItem(item.id),
    onSubmitted: onSubmitted,
  );

  Future<void> _removeItem(String id) async {
    await widget.core.removeItem(id);
    _newBlankItems.remove(id);
    // Keep the detached node until this note closes; disposing it here can
    // race the stream rebuild that unmounts its row.
    _itemRowKeys.remove(id);
  }

  Future<void> _insertItemAfter(String itemId) async {
    final before = widget.core.current.items;
    if (!before.any((item) => item.id == itemId)) return;

    final insertedId = await widget.core.addItem('');
    _newBlankItems.add(insertedId);
    final current = widget.core.current.items;
    final index = current.indexWhere((item) => item.id == itemId);
    if (index >= 0) await widget.core.moveItem(insertedId, index + 1);

    final focus = _focusForItem(insertedId);
    focus.requestFocus();
  }

  _BodyField _bodyField(NoteView note, {bool expands = false}) => _BodyField(
    // Not keyed on the text, unlike the title: the field keeps its own
    // controller across remote edits so the caret stays where the person left
    // it. See [_BodyFieldState.didUpdateWidget].
    key: const Key('body-field'),
    body: note.body,
    // A checklist only has a body when a merge left one there, so it is
    // prompted for as what it is rather than as the place to start typing.
    hint: note.kind == NoteKind.text ? 'Note' : 'Text',
    expands: expands,
    onEdit: widget.core.editBody,
  );

  /// A text note: the body, and under it any items a merge left on it — drawn,
  /// because the rule is that a merge never hides anything, but not reorderable,
  /// because switching to a checklist is the way to work on them as a list.
  Widget _text(NoteView note) => LayoutBuilder(
    builder: (context, constraints) {
      // Give the body the unused viewport height so tapping its blank area
      // focuses the editor, while keeping room for any items a merge left here.
      final bodyHeight = (constraints.maxHeight - note.items.length * 56 - 16)
          .clamp(160.0, double.infinity)
          .toDouble();
      return ListView(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        children: [
          SizedBox(height: bodyHeight, child: _bodyField(note, expands: true)),
          for (final item in note.items) _row(item),
        ],
      );
    },
  );

  /// A checklist, with any body a merge left on it above the items.
  Widget _checklist(NoteView note) {
    final active = note.items.where((item) => !item.checked).toList();
    final crossedOff = note.items.where((item) => item.checked).toList();
    final addIndex = active.length;
    final crossedHeaderIndex = addIndex + 1;
    final checkedStartIndex = crossedHeaderIndex + 1;
    final itemCount =
        addIndex +
        1 +
        (crossedOff.isEmpty
            ? 0
            : 1 + (_crossedOffExpanded ? crossedOff.length : 0));

    return ReorderableListView.builder(
      buildDefaultDragHandles: false,
      padding: const EdgeInsets.only(top: 8, bottom: 16),
      header: note.body.isEmpty
          ? note.items.isEmpty
                ? const Center(
                    key: Key('empty-state'),
                    child: Padding(
                      padding: EdgeInsets.all(24),
                      child: Text('Nothing on this checklist yet.'),
                    ),
                  )
                : null
          : Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: _bodyField(note),
            ),
      itemCount: itemCount,
      // Only unchecked rows have drag handles. Convert the target position in
      // that visible group back to the core's complete item order.
      onReorderItem: (oldIndex, newIndex) {
        if (oldIndex >= active.length) return;
        final moving = active[oldIndex];
        final remaining = note.items
            .where((item) => item.id != moving.id)
            .toList();
        final remainingActive = remaining
            .where((item) => !item.checked)
            .toList();
        if (remainingActive.isEmpty) return;
        final target = newIndex.clamp(0, remainingActive.length).toInt();
        final coreIndex = target < remainingActive.length
            ? remaining.indexWhere(
                (item) => item.id == remainingActive[target].id,
              )
            : remaining.indexWhere(
                    (item) => item.id == remainingActive.last.id,
                  ) +
                  1;
        widget.core.moveItem(moving.id, coreIndex);
      },
      itemBuilder: (context, index) {
        if (index < active.length) {
          final item = active[index];
          return _row(
            item,
            reorderIndex: index,
            onSubmitted: () => _insertItemAfter(item.id),
          );
        }
        if (index == addIndex) {
          return KeyedSubtree(
            key: const ValueKey('add-list-entry-row'),
            child: ListTile(
              key: const Key('add-list-entry'),
              contentPadding: const EdgeInsets.symmetric(horizontal: 8),
              minLeadingWidth: 0,
              leading: const SizedBox(width: 88),
              title: const Text('+ Checklist item'),
              onTap: _addListEntry,
            ),
          );
        }
        if (crossedOff.isNotEmpty && index == crossedHeaderIndex) {
          return ListTile(
            key: const ValueKey('crossed-off-items'),
            title: Text('Crossed off items (${crossedOff.length})'),
            trailing: Icon(
              _crossedOffExpanded ? Icons.expand_less : Icons.expand_more,
            ),
            onTap: _toggleCrossedOff,
          );
        }
        final item = crossedOff[index - checkedStartIndex];
        return _row(item, onSubmitted: () => _insertItemAfter(item.id));
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<NoteView>(
      stream: widget.core.watch(),
      initialData: widget.core.current,
      builder: (context, snapshot) {
        final note = snapshot.data ?? const NoteView.empty();
        final library = widget.library;
        if (library == null) return _screen(note, null);
        return StreamBuilder<List<NoteSummary>>(
          stream: _noteSummaries,
          initialData: library.notes,
          builder: (context, summaries) {
            final summary = summaries.data
                ?.where((summary) => summary.id == widget.noteId)
                .firstOrNull;
            if (summaries.hasData && summary == null) {
              if (!_deletionPopScheduled) {
                _deletionPopScheduled = true;
                WidgetsBinding.instance.addPostFrameCallback((_) {
                  if (!mounted) return;
                  ScaffoldMessenger.maybeOf(context)?.showSnackBar(
                    const SnackBar(
                      content: Text('This note was deleted from your account.'),
                    ),
                  );
                  Navigator.of(context).maybePop();
                });
              }
              return const Scaffold(
                body: Center(
                  child: Text('This note was deleted from your account.'),
                ),
              );
            }
            return _screen(note, summary);
          },
        );
      },
    );
  }

  Widget _screen(NoteView note, NoteSummary? summary) {
    final isText = note.kind == NoteKind.text;
    final library = widget.library;
    final tint = noteColourTint(context, summary?.colour);
    return PopScope<Object?>(
      canPop: _allowPop || (_pendingItemWrites == 0 && _newBlankItems.isEmpty),
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) unawaited(_popAfterItemWrites());
      },
      child: Scaffold(
        backgroundColor: tint,
        appBar: AppBar(
          backgroundColor: tint,
          title: null,
          actions: [
            IconButton(
              key: const Key('share-button'),
              icon: const Icon(Icons.person_add_outlined),
              tooltip: 'Share',
              onPressed: _share,
            ),
          ],
        ),
        body: Column(
          children: [
            StorageBanner(storage: widget.storage),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
              child: _TitleField(
                key: ValueKey('title:${note.title}'),
                title: note.title,
                hint: isText ? 'Title' : 'Untitled checklist',
                onChanged: widget.core.setTitle,
              ),
            ),
            Expanded(child: isText ? _text(note) : _checklist(note)),
          ],
        ),
        bottomNavigationBar: library == null || summary == null
            ? _KindToggleBar(
                onSend: () => _shareContent(widget.core.current),
                isText: isText,
                onKindToggle: () => widget.core.setKind(
                  isText ? NoteKind.checklist : NoteKind.text,
                ),
              )
            : _NoteToolbar(
                onSend: () => _shareContent(widget.core.current),
                note: summary,
                library: library,
                isText: isText,
                onKindToggle: () => widget.core.setKind(
                  isText ? NoteKind.checklist : NoteKind.text,
                ),
                onLabels: widget.onLabels,
                onReminders: widget.onReminders,
                onTrash: () async {
                  await library.setTrashed(summary.id, !summary.trashed);
                  if (mounted) Navigator.of(context).pop();
                },
              ),
      ),
    );
  }
}

class _NoteToolbar extends StatelessWidget {
  const _NoteToolbar({
    required this.onSend,
    required this.note,
    required this.library,
    required this.isText,
    required this.onKindToggle,
    required this.onTrash,
    this.onLabels,
    this.onReminders,
  });

  final NoteSummary note;
  final Future<void> Function() onSend;
  final NoteLibrary library;
  final bool isText;
  final VoidCallback onKindToggle;
  final Future<void> Function() onTrash;
  final Future<void> Function()? onLabels;
  final Future<void> Function()? onReminders;

  Future<void> _chooseColour(BuildContext context) =>
      showModalBottomSheet<void>(
        context: context,
        builder: (context) => SafeArea(
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12),
            child: SizedBox(
              height: 64,
              child: ListView(
                scrollDirection: Axis.horizontal,
                padding: EdgeInsets.zero,
                children: [
                  _ColourChoice(
                    colour: null,
                    selected: note.colour == null,
                    onTap: () async {
                      Navigator.of(context).pop();
                      await library.setColour(note.id, null);
                    },
                  ),
                  for (final colour in NoteColour.values)
                    _ColourChoice(
                      colour: colour,
                      selected: note.colour == colour,
                      onTap: () async {
                        Navigator.of(context).pop();
                        await library.setColour(note.id, colour);
                      },
                    ),
                ],
              ),
            ),
          ),
        ),
      );

  Future<void> _showActions(BuildContext context) => showModalBottomSheet<void>(
    context: context,
    builder: (sheetContext) {
      void run(Future<void> Function() action) {
        Navigator.of(sheetContext).pop();
        unawaited(action());
      }

      return SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (!note.trashed)
              ListTile(
                leading: Icon(
                  note.pinned ? Icons.push_pin : Icons.push_pin_outlined,
                ),
                title: Text(note.pinned ? 'Unpin' : 'Pin'),
                onTap: () =>
                    run(() => library.setPinned(note.id, !note.pinned)),
              ),
            if (!note.trashed && onLabels != null)
              ListTile(
                leading: const Icon(Icons.label_outline),
                title: const Text('Labels'),
                onTap: () => run(onLabels!),
              ),
            if (!note.trashed && onReminders != null)
              ListTile(
                leading: const Icon(Icons.notifications_outlined),
                title: const Text('Reminders'),
                onTap: () => run(onReminders!),
              ),
            ListTile(
              key: const Key('share-content-button'),
              leading: const Icon(Icons.ios_share),
              title: const Text('Send a copy'),
              onTap: () => run(onSend),
            ),
            if (!note.trashed)
              ListTile(
                leading: Icon(
                  note.archived
                      ? Icons.unarchive_outlined
                      : Icons.archive_outlined,
                ),
                title: Text(note.archived ? 'Unarchive' : 'Archive'),
                onTap: () => run(() async {
                  await library.setArchived(note.id, !note.archived);
                  if (context.mounted) Navigator.of(context).pop();
                }),
              ),
            ListTile(
              leading: Icon(
                note.trashed ? Icons.restore_from_trash : Icons.delete_outline,
              ),
              title: Text(note.trashed ? 'Restore' : 'Move to trash'),
              onTap: () => run(onTrash),
            ),
          ],
        ),
      );
    },
  );

  @override
  Widget build(BuildContext context) => SafeArea(
    child: Material(
      color:
          noteColourTint(context, note.colour) ??
          Theme.of(context).colorScheme.surface,
      child: Row(
        children: [
          IconButton(
            key: const Key('colour-button'),
            tooltip: 'Change colour',
            icon: const Icon(Icons.palette_outlined),
            onPressed: () => _chooseColour(context),
          ),
          IconButton(
            key: const Key('kind-toggle'),
            icon: Icon(
              isText ? Icons.check_box_outlined : Icons.notes_outlined,
            ),
            tooltip: isText ? 'Show checkboxes' : 'Hide checkboxes',
            onPressed: onKindToggle,
          ),
          const Spacer(),
          IconButton(
            key: const Key('note-more-menu'),
            tooltip: 'More note actions',
            icon: const Icon(Icons.more_vert),
            onPressed: () => _showActions(context),
          ),
        ],
      ),
    ),
  );
}

class _KindToggleBar extends StatelessWidget {
  const _KindToggleBar({
    required this.isText,
    required this.onKindToggle,
    required this.onSend,
  });

  final bool isText;
  final VoidCallback onKindToggle;
  final Future<void> Function() onSend;

  @override
  Widget build(BuildContext context) => SafeArea(
    child: Row(
      children: [
        IconButton(
          key: const Key('kind-toggle'),
          icon: Icon(isText ? Icons.check_box_outlined : Icons.notes_outlined),
          tooltip: isText ? 'Show checkboxes' : 'Hide checkboxes',
          onPressed: onKindToggle,
        ),
        const Spacer(),
        IconButton(
          key: const Key('note-more-menu'),
          tooltip: 'More note actions',
          icon: const Icon(Icons.more_vert),
          onPressed: () => showModalBottomSheet<void>(
            context: context,
            builder: (context) => SafeArea(
              child: ListTile(
                key: const Key('share-content-button'),
                leading: const Icon(Icons.ios_share),
                title: const Text('Send a copy'),
                onTap: () {
                  Navigator.of(context).pop();
                  unawaited(onSend());
                },
              ),
            ),
          ),
        ),
      ],
    ),
  );
}

class _ColourChoice extends StatelessWidget {
  const _ColourChoice({
    required this.colour,
    required this.selected,
    required this.onTap,
  });

  final NoteColour? colour;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Semantics(
    button: true,
    selected: selected,
    label: colour == null ? 'No colour' : '${colour!.name} colour',
    child: Tooltip(
      message: colour == null ? 'No colour' : colour!.name,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 8),
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: onTap,
          child: Container(
            width: 48,
            height: 48,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color:
                  noteColourTint(context, colour) ??
                  Theme.of(context).colorScheme.surface,
              border: Border.all(
                color: selected
                    ? Theme.of(context).colorScheme.primary
                    : Theme.of(context).dividerColor,
                width: selected ? 3 : 1,
              ),
            ),
            child: colour == null
                ? const Icon(Icons.format_color_reset_outlined, size: 20)
                : null,
          ),
        ),
      ),
    ),
  );
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
    this.expands = false,
    required this.onEdit,
  });

  final String body;
  final String hint;
  final bool expands;
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

  /// Bodies those sends started from. The core call can finish before its
  /// updated view reaches this widget, so a completion-time read may still be
  /// one of these older bodies. Never adopt one over newer text in the field.
  final List<String> _sentFrom = [];
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
        _sentFrom.add(from);
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
      _sentFrom.removeRange(0, own + 1);
      return;
    }
    if (_sentFrom.contains(body)) {
      // This is a view from before one of our in-flight edits, not a remote
      // change. Adopting it would move the caret back as the person types.
      return;
    }
    if (body == _controller.text) return;
    // Someone else's change, or theirs merged with this field's: either way
    // it is what the core now holds, and what the next send changes from.
    _sent.clear();
    _sentFrom.clear();
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
      expands: widget.expands,
      minLines: widget.expands ? null : 1,
      maxLines: null,
      decoration: InputDecoration(
        hintText: widget.hint,
        border: InputBorder.none,
      ),
      onChanged: _send,
    );
  }
}

/// One item row: drag handle when reorderable, checkbox, editable text, delete.
class _ItemRow extends StatefulWidget {
  const _ItemRow({
    super.key,
    required this.item,
    required this.focusNode,
    this.reorderIndex,
    required this.onChecked,
    required this.onTextChanged,
    required this.onWriteStateChanged,
    required this.onDelete,
    this.onSubmitted,
  });

  final ItemView item;
  final FocusNode focusNode;
  final int? reorderIndex;
  final ValueChanged<bool?> onChecked;
  final Future<void> Function(String) onTextChanged;
  final ValueChanged<bool> onWriteStateChanged;
  final VoidCallback onDelete;
  final Future<void> Function()? onSubmitted;

  @override
  State<_ItemRow> createState() => _ItemRowState();
}

class _ItemRowState extends State<_ItemRow> {
  late final _controller = TextEditingController(text: widget.item.text);
  late String _lastWritten = widget.item.text;
  FocusNode get _focus => widget.focusNode;
  bool _submitting = false;
  bool _writing = false;
  String? _queuedText;
  Completer<void>? _writeDone;

  @override
  void initState() {
    super.initState();
    _focus.addListener(_onFocusChange);
  }

  void _onFocusChange() {
    if (mounted) setState(() {});
    if (!_focus.hasFocus && !_submitting) unawaited(_commit());
  }

  Future<void> _write(String text) async {
    if (_writing) {
      _queuedText = text;
      return;
    }
    _writing = true;
    widget.onWriteStateChanged(true);
    final done = Completer<void>();
    _writeDone = done;
    try {
      String? next = text;
      while (next != null) {
        _queuedText = null;
        if (next != _lastWritten) {
          await widget.onTextChanged(next);
          _lastWritten = next;
        }
        next = _queuedText;
      }
    } finally {
      _writing = false;
      _writeDone = null;
      done.complete();
      widget.onWriteStateChanged(false);
    }
  }

  Future<void> _commit() async {
    final text = _controller.text;
    if (_writing) {
      _queuedText = text;
      await _writeDone?.future;
    } else if (text != _lastWritten) {
      await _write(text);
    }
  }

  Future<void> _submit() async {
    if (_submitting) return;
    _submitting = true;
    try {
      await _commit();
      await widget.onSubmitted?.call();
    } finally {
      _submitting = false;
    }
  }

  @override
  void didUpdateWidget(_ItemRow old) {
    super.didUpdateWidget(old);
    // The text changed underneath us — someone else edited this item. Take the
    // new value, unless this device is the one typing into it.
    if (widget.item.text != old.item.text && !_focus.hasFocus) {
      _controller.text = widget.item.text;
      _lastWritten = widget.item.text;
    }
  }

  @override
  void dispose() {
    _focus.removeListener(_onFocusChange);
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final item = widget.item;
    return ListTile(
      contentPadding: const EdgeInsets.symmetric(horizontal: 8),
      minLeadingWidth: 0,
      leading: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (widget.reorderIndex != null)
            ReorderableDragStartListener(
              index: widget.reorderIndex!,
              child: const SizedBox(
                width: 40,
                height: 48,
                child: Icon(Icons.drag_handle),
              ),
            ),
          Checkbox(
            key: Key('check:${item.id}'),
            value: item.checked,
            onChanged: widget.onChecked,
          ),
        ],
      ),
      title: TextField(
        key: Key('text:${item.id}'),
        controller: _controller,
        focusNode: _focus,
        textInputAction: widget.onSubmitted == null
            ? null
            : TextInputAction.next,
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
        onChanged: _write,
        onSubmitted: widget.onSubmitted == null
            ? (_) => _commit()
            : (_) => _submit(),
      ),
      trailing: _focus.hasFocus
          ? IconButton(
              key: Key('delete:${item.id}'),
              icon: const Icon(Icons.close),
              tooltip: 'Delete',
              onPressed: widget.onDelete,
            )
          : null,
    );
  }
}

class _ContentShareDialog extends StatefulWidget {
  const _ContentShareDialog({required this.text, required this.format});

  final String text;
  final String format;

  @override
  State<_ContentShareDialog> createState() => _ContentShareDialogState();
}

class _ContentShareDialogState extends State<_ContentShareDialog> {
  bool _fallback = false;

  Future<void> _share() async {
    final shared = await shareContent(widget.text);
    if (!mounted) return;
    if (shared) {
      Navigator.of(context).pop();
    } else {
      setState(() => _fallback = true);
    }
  }

  Future<void> _copy() async {
    try {
      await Clipboard.setData(ClipboardData(text: widget.text));
      if (!mounted) return;
      final messenger = ScaffoldMessenger.maybeOf(context);
      Navigator.of(context).pop();
      messenger?.showSnackBar(const SnackBar(content: Text('Content copied')));
    } catch (_) {
      if (mounted) setState(() => _fallback = true);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text('Share as ${widget.format}'),
    content: SizedBox(
      width: 440,
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            if (_fallback)
              const Padding(
                padding: EdgeInsets.only(bottom: 12),
                child: Text(
                  'Sharing or clipboard access is unavailable. Select and copy the content below.',
                ),
              ),
            SelectableText(
              widget.text.isEmpty ? 'This note has no content.' : widget.text,
              key: const Key('share-content-preview'),
            ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        key: const Key('share-content-native'),
        onPressed: widget.text.isEmpty ? null : _share,
        child: const Text('Share'),
      ),
      TextButton(
        key: const Key('copy-note-content'),
        onPressed: widget.text.isEmpty ? null : _copy,
        child: const Text('Copy'),
      ),
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Done'),
      ),
    ],
  );
}
