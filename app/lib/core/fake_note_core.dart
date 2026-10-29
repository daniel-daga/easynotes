/// An in-memory [NoteCore] for widget tests.
///
/// It is *not* a CRDT and does not pretend to be: convergence is tested in Rust,
/// where it belongs. What this has to get right is the contract the screen
/// depends on — edits go in, whole views come out on the stream, and the order
/// keys behave like the real ones.
library;

import 'dart:async';
import 'dart:typed_data';

import 'note_core.dart';
import 'note_view.dart';
import 'sync_status.dart';

class FakeNoteCore implements NoteCore {
  FakeNoteCore({
    String title = '',
    List<String> items = const [],
    NoteKind kind = NoteKind.checklist,
    String body = '',
  }) {
    _title = title;
    _kind = kind;
    _body = body;
    for (final text in items) {
      _items.add(_Item(id: _nextId(), text: text, order: _appendKey()));
    }
    _emit();
  }

  final _controller = StreamController<NoteView>.broadcast();
  final _sync = StreamController<SyncStatus>.broadcast();
  final List<_Item> _items = [];
  String _title = '';
  NoteKind _kind = NoteKind.checklist;
  String _body = '';
  int _counter = 0;

  /// The revision of the newest view. Bumped by every publish, as the real
  /// core's counter is, so a field comparing revisions behaves the same here.
  int _rev = 0;

  /// The revision of the newest view, for a test that has to hand one in.
  int get rev => _rev;

  /// Every edit the screen has asked for, in order. Tests assert on this to show
  /// that a gesture reached the core rather than only changing pixels.
  final List<String> edits = [];

  NoteView _view = const NoteView.empty();
  SyncStatus _status = const SyncStatus.offline();

  @override
  NoteView get current => _view;

  @override
  SyncStatus get syncStatus => _status;

  @override
  Stream<NoteView> watch() async* {
    yield _view;
    yield* _controller.stream;
  }

  @override
  Future<void> setTitle(String title) async {
    edits.add('setTitle($title)');
    _title = title;
    _emit();
  }

  /// Takes [to] as it is: rebasing onto a body that moved is the real core's
  /// job, and is tested there.
  @override
  Future<int> editBody(String from, String to) async {
    if (to == _body) return _rev;
    edits.add('editBody($from -> $to)');
    _body = to;
    return _emit().rev;
  }

  /// The same carrying-across the real core does, so a screen test sees what a
  /// person would: lines with something on them become unchecked items after
  /// any items already there, and items become lines after any body.
  @override
  Future<void> setKind(NoteKind kind) async {
    if (kind == _kind) return;
    edits.add('setKind(${kind.name})');
    switch (kind) {
      case NoteKind.checklist:
        for (final line in _body.split('\n')) {
          if (line.trim().isEmpty) continue;
          final task = _parseTaskLine(line);
          final item = _Item(id: _nextId(), text: task.$1, order: _appendKey())
            ..checked = task.$2;
          _items.add(item);
        }
        _body = '';
      case NoteKind.text:
        final kept = _body.replaceFirst(RegExp(r'\n+$'), '');
        _body = [
          if (kept.isNotEmpty) kept,
          for (final i in _items) i.text,
        ].join('\n');
        _items.clear();
    }
    _kind = kind;
    _emit();
  }

  @override
  Future<String> addItem(String text) async {
    edits.add('addItem($text)');
    final item = _Item(id: _nextId(), text: text, order: _appendKey());
    _items.add(item);
    _emit();
    return item.id;
  }

  @override
  Future<int> setItemText(String itemId, String text) async {
    edits.add('setItemText($itemId, $text)');
    _find(itemId).text = text;
    return _emit().rev;
  }

  @override
  Future<int> editItemText(String itemId, String from, String to) async {
    // What the real core does, in miniature: the field's splice is replayed
    // onto what the line says now, so an edit made while somebody else wrote
    // the same line keeps both. Recorded like [setItemText] because that is
    // what a test means by it.
    edits.add('setItemText($itemId, $to)');
    final item = _find(itemId);
    item.text = _replay(from, to, item.text);
    return _emit().rev;
  }

  /// Somebody else's edit to an item, landing in the core the way one from the
  /// relay does: the line says [text] now, and a view follows. Not recorded in
  /// [edits], because this device did not make it.
  Future<void> remoteItemText(String itemId, String text) async {
    _find(itemId).text = text;
    _emit();
  }

  @override
  Future<void> setChecked(String itemId, bool checked) async {
    edits.add('setChecked($itemId, $checked)');
    _find(itemId).checked = checked;
    _emit();
  }

  @override
  Future<void> moveItem(String itemId, int toIndex) async {
    edits.add('moveItem($itemId, $toIndex)');
    final item = _find(itemId);
    _items.remove(item);
    _items.insert(toIndex.clamp(0, _items.length), item);
    // Re-key so the emitted order keys stay consistent with the sequence, the
    // way the real core's fractional indices do.
    for (var i = 0; i < _items.length; i++) {
      _items[i].order = _key(i);
    }
    _emit();
  }

  @override
  Future<void> removeItem(String itemId) async {
    edits.add('removeItem($itemId)');
    _items.removeWhere((i) => i.id == itemId);
    _emit();
  }

  @override
  Future<void> dropIfBlank(String itemId) async {
    // What the real core does, in miniature: hidden while blank, so a peer's
    // text arriving later brings it back.
    edits.add('dropIfBlank($itemId)');
    if (_find(itemId).text.trim().isEmpty) _items.removeWhere((i) => i.id == itemId);
    _emit();
  }

  // -- attachments: live at once, as in a note that does not sync ----------

  final List<AttachmentView> _attachments = [];
  final Map<String, Uint8List> _bytes = {};
  final Map<String, Uint8List> _thumbnails = {};

  /// Imports that stay pending, for a test of what a screen shows while an
  /// upload waits. Pending imports are not published until [publishPending].
  bool holdImports = false;
  final List<AttachmentView> _pending = [];

  @override
  Future<AttachmentView> importAttachment(
    String name,
    Uint8List bytes, {
    Uint8List? thumbnail,
    int? durationMs,
  }) async {
    edits.add('importAttachment($name)');
    final image =
        bytes.length >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 ||
        bytes.length >= 4 && bytes[0] == 0x89 && bytes[1] == 0x50;
    final view = AttachmentView(
      id: 'attachment-${_counter++}',
      kind: image ? AttachmentKind.image : AttachmentKind.audio,
      name: name,
      mime: image ? 'image/png' : 'audio/mpeg',
      size: bytes.length,
      durationMs: durationMs,
      thumbnailWidth: thumbnail == null ? null : 64,
      thumbnailHeight: thumbnail == null ? null : 64,
      pending: holdImports,
    );
    _bytes[view.id] = bytes;
    if (thumbnail != null) _thumbnails[view.id] = thumbnail;
    if (holdImports) {
      _pending.add(view);
    } else {
      _attachments.add(view);
      _emit();
    }
    return view;
  }

  /// Publish every held import, as the queue does once they are uploaded.
  void publishPending() {
    for (final view in _pending) {
      _attachments.add(_copy(view, pending: false));
    }
    _pending.clear();
    _emit();
  }

  @override
  Future<AttachmentView> importAttachmentFile(
    String path,
    String name, {
    Uint8List? thumbnail,
    int? durationMs,
  }) => throw UnsupportedError('the fake has no disk');

  @override
  Future<List<AttachmentView>> pendingAttachments() async =>
      List.unmodifiable(_pending);

  @override
  Future<Uint8List?> readAttachment(
    String id, {
    bool thumbnail = false,
  }) async => thumbnail ? _thumbnails[id] : _bytes[id];

  @override
  Future<void> removeAttachment(String id) async {
    edits.add('removeAttachment($id)');
    _attachments.removeWhere((a) => a.id == id);
    _pending.removeWhere((a) => a.id == id);
    _bytes.remove(id);
    _thumbnails.remove(id);
    _emit();
  }

  @override
  Future<void> moveAttachment(String id, int toIndex) async {
    edits.add('moveAttachment($id, $toIndex)');
    final index = _attachments.indexWhere((a) => a.id == id);
    if (index < 0) return;
    final moved = _attachments.removeAt(index);
    _attachments.insert(toIndex.clamp(0, _attachments.length), moved);
    _emit();
  }

  @override
  Future<void> describeAttachment(String id, String description) async {
    edits.add('describeAttachment($id, $description)');
    final index = _attachments.indexWhere((a) => a.id == id);
    if (index < 0) return;
    _attachments[index] = _copy(_attachments[index], description: description);
    _emit();
  }

  static AttachmentView _copy(
    AttachmentView a, {
    bool? pending,
    String? description,
  }) => AttachmentView(
    id: a.id,
    kind: a.kind,
    name: a.name,
    mime: a.mime,
    size: a.size,
    width: a.width,
    height: a.height,
    durationMs: a.durationMs,
    description: description ?? a.description,
    thumbnailWidth: a.thumbnailWidth,
    thumbnailHeight: a.thumbnailHeight,
    pending: pending ?? a.pending,
  );

  @override
  Stream<SyncStatus> watchSync() async* {
    yield _status;
    yield* _sync.stream;
  }

  /// Say what the engine would say. Tests use it to put the screen in a state
  /// the real core would take a network outage to reach.
  void pushStatus(SyncStatus status) {
    _status = status;
    _sync.add(status);
  }

  /// How many times the screen has let go of this note. Every open hands out a
  /// handle, and a handle never let go of leaves a Rust task per visit.
  int disposals = 0;

  @override
  Future<void> dispose() async {
    disposals++;
    await _controller.close();
    await _sync.close();
  }

  /// Push a view the screen did not ask for, as a remote edit from the relay
  /// would. Tests use it to prove the screen really does render from the stream.
  ///
  /// The revision is bumped unless the test names one, because a view at an old
  /// revision is an echo the screen ignores — and a test that wants to prove
  /// that passes [rev] explicitly.
  void pushRemote(NoteView view, {int? rev}) {
    _rev = rev ?? _rev + 1;
    _view = _at(view, _rev);
    _controller.add(_view);
  }

  /// The view published, with the revision it is at.
  NoteView _emit() {
    _rev++;
    _view = NoteView(
      rev: _rev,
      kind: _kind,
      title: _title,
      body: _body,
      items: [
        for (final i in _items)
          ItemView(id: i.id, text: i.text, checked: i.checked, order: i.order),
      ],
      attachments: List.unmodifiable(_attachments),
    );
    _controller.add(_view);
    return _view;
  }

  static NoteView _at(NoteView v, int rev) => NoteView(
    rev: rev,
    kind: v.kind,
    title: v.title,
    body: v.body,
    colour: v.colour,
    colourSet: v.colourSet,
    items: v.items,
    attachments: v.attachments,
  );

  _Item _find(String id) => _items.firstWhere(
    (i) => i.id == id,
    orElse: () => throw UnknownItemException(id),
  );

  String _nextId() => 'item-${_counter++}';

  String _appendKey() => _key(_items.length);

  /// Zero-padded so string comparison matches numeric order, as the real
  /// fractional index keys do.
  String _key(int i) => i.toString().padLeft(4, '0');
}

/// [text]'s one splice from [base], replayed at the same place in [current].
///
/// A miniature of the core's `text::rebase`, enough for the short lines a
/// widget test types: where the splice starts in the base is where it starts
/// in the current text, provided the change before it is an append.
String _replay(String base, String text, String current) {
  if (base == current) return text;
  final at = _commonPrefix(base, text);
  final tail = _commonSuffix(base, text, at);
  final inserted = text.substring(at, text.length - tail);
  final ahead = _commonPrefix(base, current);
  final start = ahead >= at ? at : ahead;
  return current.substring(0, start) + inserted + current.substring(start);
}

int _commonPrefix(String a, String b) {
  var i = 0;
  while (i < a.length && i < b.length && a.codeUnitAt(i) == b.codeUnitAt(i)) {
    i++;
  }
  return i;
}

int _commonSuffix(String a, String b, int prefix) {
  var i = 0;
  while (i < a.length - prefix &&
      i < b.length - prefix &&
      a.codeUnitAt(a.length - 1 - i) == b.codeUnitAt(b.length - 1 - i)) {
    i++;
  }
  return i;
}

(String, bool) _parseTaskLine(String line) {
  final match = RegExp(
    r'^\s*(?:[-*+]\s+)?\[([ xX])\](?:\s(.*))?$',
  ).firstMatch(line);
  if (match == null) return (line, false);
  return ((match.group(2) ?? '').trim(), match.group(1)!.toLowerCase() == 'x');
}

class _Item {
  _Item({required this.id, required this.text, required this.order});

  final String id;
  String text;
  bool checked = false;
  String order;
}
