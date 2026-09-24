/// An in-memory [NoteCore] for widget tests.
///
/// It is *not* a CRDT and does not pretend to be: convergence is tested in Rust,
/// where it belongs. What this has to get right is the contract the screen
/// depends on — edits go in, whole views come out on the stream, and the order
/// keys behave like the real ones.
library;

import 'dart:async';

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
  Future<void> editBody(String from, String to) async {
    if (to == _body) return;
    edits.add('editBody($from -> $to)');
    _body = to;
    _emit();
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
          final item = _Item(
            id: _nextId(),
            text: task.$1,
            order: _appendKey(),
          )..checked = task.$2;
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
  Future<void> setItemText(String itemId, String text) async {
    edits.add('setItemText($itemId, $text)');
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
  Future<String> exportShareBlob() async {
    edits.add('exportShareBlob()');
    return 'AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCk';
  }

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

  @override
  Future<void> dispose() async {
    await _controller.close();
    await _sync.close();
  }

  /// Push a view the screen did not ask for, as a remote edit from the relay
  /// would. Tests use it to prove the screen really does render from the stream.
  void pushRemote(NoteView view) {
    _view = view;
    _controller.add(view);
  }

  void _emit() {
    _view = NoteView(
      kind: _kind,
      title: _title,
      body: _body,
      items: [
        for (final i in _items)
          ItemView(id: i.id, text: i.text, checked: i.checked, order: i.order),
      ],
    );
    _controller.add(_view);
  }

  _Item _find(String id) => _items.firstWhere(
    (i) => i.id == id,
    orElse: () => throw StateError('no item $id'),
  );

  String _nextId() => 'item-${_counter++}';

  String _appendKey() => _key(_items.length);

  /// Zero-padded so string comparison matches numeric order, as the real
  /// fractional index keys do.
  String _key(int i) => i.toString().padLeft(4, '0');
}

(String, bool) _parseTaskLine(String line) {
  final match = RegExp(r'^[-*+]\s+\[([ xX])\]\s?(.*)$').firstMatch(line);
  if (match == null) return (line, false);
  return (match.group(2)!.trim(), match.group(1)!.toLowerCase() == 'x');
}

class _Item {
  _Item({required this.id, required this.text, required this.order});

  final String id;
  String text;
  bool checked = false;
  String order;
}
