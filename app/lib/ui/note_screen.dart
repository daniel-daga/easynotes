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

import 'package:audioplayers/audioplayers.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/attachment_files.dart';
import '../core/content_interchange.dart';
import '../core/content_sharing.dart';
import '../core/note_core.dart';
import '../core/note_library.dart';
import '../core/note_view.dart';
import 'attachments.dart';
import 'invites.dart';
import 'live_text.dart';
import 'note_appearance.dart';
import 'storage_banner.dart';

class NoteScreen extends StatefulWidget {
  const NoteScreen({
    super.key,
    required this.core,
    required this.storage,
    required this.noteId,
    required this.invites,
    this.autofocusBody = false,
    this.library,
    this.onLabels,
    this.onReminders,
    this.files = const DeviceAttachmentFiles(),
    this.relayUrl,
    this.relayAccess,
  });

  final NoteCore core;

  /// The relay this note lives on, when this device has one. Invites made here
  /// carry it, so a guest needs no second link.
  final String? relayUrl;

  /// That relay's access key, when it has one.
  final String? relayAccess;

  /// Where attachments are picked from and saved to.
  final AttachmentFiles files;

  /// Which note this is, for the invite made from here.
  final String noteId;

  /// How this note is shared with another person. The device's, like
  /// [storage], because an invite lives on the relay the device talks to.
  final Invites invites;

  /// Put the cursor in the body when opening a freshly created text note.
  final bool autofocusBody;

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

  /// Items Enter has made that the core has not answered for yet. Each is
  /// drawn at once with the keyboard in it, under a stand-in ID; see
  /// [_insertItemAfter].
  final List<_PendingItem> _pending = [];
  int _pendingCount = 0;

  /// Every stand-in ID the core has since answered for, and with what.
  final Map<String, String> _answered = {};

  /// The other way round: the stand-in a real item's row was first drawn
  /// under, which its text field keeps as its key. A new key would be a new
  /// field, and a new field is the keyboard moving.
  final Map<String, String> _standInFor = {};
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

  // -- attachments ----------------------------------------------------------

  /// This device's imports into the note that are not in it yet, as the core
  /// last said. Asked again whenever the note or a transfer changes, which is
  /// when one can have been published.
  List<AttachmentView> _pendingAttachments = const [];

  /// Files chosen and still being prepared: read, previewed and sealed. Shown
  /// at once, so a large file is not a silence.
  final List<String> _preparing = [];

  /// The transfers that concern this note.
  List<AttachmentTransfer> _transfers = const [];
  StreamSubscription<List<AttachmentTransfer>>? _transferWatch;
  StreamSubscription<NoteView>? _viewWatch;
  AudioPlayer? _audioPlayer;
  StreamSubscription<PlayerState>? _playerStateWatch;
  StreamSubscription<Duration>? _playerPositionWatch;
  StreamSubscription<Duration?>? _playerDurationWatch;
  StreamSubscription<void>? _playerCompleteWatch;
  String? _playingAttachment;
  String? _loadingAttachment;
  String? _playbackUnavailableAttachment;
  Duration _playbackPosition = Duration.zero;
  Duration? _playbackDuration;
  bool _playbackIsPlaying = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) unawaited(_startEmptyChecklist());
    });
    _viewWatch = widget.core.watch().listen((note) {
      _refreshPending();
      final id = _playingAttachment;
      if (id != null && !note.attachments.any((a) => a.id == id)) {
        unawaited(_stopPlayback());
      }
    });
    _transferWatch = widget.library?.watchTransfers().listen((transfers) {
      if (!mounted) return;
      setState(
        () => _transfers = [
          for (final t in transfers)
            if (t.note == widget.noteId) t,
        ],
      );
      _refreshPending();
    });
  }

  Future<void> _refreshPending() async {
    try {
      final pending = await widget.core.pendingAttachments();
      if (!mounted) return;
      if (!listEquals(pending, _pendingAttachments)) {
        setState(() => _pendingAttachments = pending);
      }
      final id = _playingAttachment;
      if (id != null &&
          !widget.core.current.attachments.any((a) => a.id == id) &&
          !pending.any((a) => a.id == id)) {
        unawaited(_stopPlayback());
      }
    } on Object {
      // The note has gone or the library closed; the screen is leaving.
    }
  }

  void _say(String message) => ScaffoldMessenger.maybeOf(
    context,
  )?.showSnackBar(SnackBar(content: Text(message)));

  Future<void> _addAttachment() async {
    final choice = await showModalBottomSheet<AttachmentKind>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              key: const Key('attach-images'),
              leading: const Icon(Icons.image_outlined),
              title: const Text('Choose images'),
              onTap: () => Navigator.of(context).pop(AttachmentKind.image),
            ),
            ListTile(
              key: const Key('attach-audio'),
              leading: const Icon(Icons.audiotrack_outlined),
              title: const Text('Audio file'),
              onTap: () => Navigator.of(context).pop(AttachmentKind.audio),
            ),
          ],
        ),
      ),
    );
    if (choice == null || !mounted) return;
    final List<PickedFile> picked;
    try {
      picked = choice == AttachmentKind.image
          ? await widget.files.pickImages()
          : await widget.files.pickAudio();
    } on Object catch (e) {
      if (mounted) _say('Couldn\'t open the file picker: $e');
      return;
    }
    // One at a time, in the order chosen, so the gallery keeps that order.
    for (final file in picked) {
      if (!mounted) return;
      setState(() => _preparing.add(file.name));
      try {
        await _import(file, image: choice == AttachmentKind.image);
      } on Object catch (e) {
        if (mounted) _say('Couldn\'t add ${file.name}: ${_reason(e)}');
      } finally {
        if (mounted) setState(() => _preparing.remove(file.name));
      }
      await _refreshPending();
    }
  }

  Future<void> _import(PickedFile file, {required bool image}) async {
    final bytes = file.bytes;
    if (bytes != null) {
      final thumbnail = image ? await widget.files.thumbnail(bytes) : null;
      await widget.core.importAttachment(
        file.name,
        bytes,
        thumbnail: thumbnail,
      );
    } else {
      await widget.core.importAttachmentFile(file.path!, file.name);
    }
  }

  /// The core's words, without the bridge's wrapping around them.
  static String _reason(Object e) {
    final text = '$e';
    const prefix = 'AnyhowException(';
    if (text.startsWith(prefix) && text.endsWith(')')) {
      return text.substring(prefix.length, text.length - 1);
    }
    return text;
  }

  void _retryTransfers() => widget.library?.retryTransfers();

  Future<void> _openImage(List<AttachmentView> images, int index) =>
      Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => AttachmentViewer(
            images: widget.core.watch().map(
              (note) => _attachments(note, AttachmentKind.image),
            ),
            initialImages: images,
            initial: index,
            read: widget.core.readAttachment,
            onAction: _attachmentAction,
          ),
        ),
      );

  Future<void> _attachmentAction(
    AttachmentView attachment,
    AttachmentAction action,
  ) async {
    switch (action) {
      case AttachmentAction.saveCopy:
        final bytes = await widget.core.readAttachment(attachment.id);
        if (!mounted) return;
        if (bytes == null) {
          _say('This file is still downloading. Try again when it is here.');
          return;
        }
        try {
          await widget.files.save(attachment.name, attachment.mime, bytes);
        } on Object catch (e) {
          if (mounted) _say('Couldn\'t save a copy: ${_reason(e)}');
        }
      case AttachmentAction.describe:
        final description = await showDialog<String>(
          context: context,
          builder: (context) =>
              _DescribeDialog(initial: attachment.description),
        );
        if (description == null) return;
        await widget.core.describeAttachment(attachment.id, description);
      case AttachmentAction.moveEarlier || AttachmentAction.moveLater:
        await _move(
          attachment,
          earlier: action == AttachmentAction.moveEarlier,
        );
      case AttachmentAction.remove:
        final confirmed = await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            title: const Text('Remove attachment?'),
            content: Text(
              attachment.pending
                  ? 'It has not been shared yet, so only this device has it.'
                  : 'Remove this attachment for everyone on this note? The '
                        'file you added it from stays where it is.',
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.of(context).pop(false),
                child: const Text('Cancel'),
              ),
              TextButton(
                key: const Key('confirm-remove-attachment'),
                onPressed: () => Navigator.of(context).pop(true),
                child: const Text('Remove'),
              ),
            ],
          ),
        );
        if (confirmed != true) return;
        await widget.core.removeAttachment(attachment.id);
        await _refreshPending();
    }
  }

  /// Move an attachment past its neighbour of the same kind. The core counts
  /// every attachment, pictures and sound files together, so the neighbour's
  /// place among all of them is the index it is asked for: before the one
  /// earlier, or — once this one is out of the way — where the later one was.
  Future<void> _move(AttachmentView attachment, {required bool earlier}) async {
    final all = widget.core.current.attachments;
    final same = [
      for (final a in all)
        if (a.kind == attachment.kind) a,
    ];
    final at = same.indexWhere((a) => a.id == attachment.id);
    final neighbour = earlier ? at - 1 : at + 1;
    if (at < 0 || neighbour < 0 || neighbour >= same.length) return;
    final target = all.indexWhere((a) => a.id == same[neighbour].id);
    await widget.core.moveAttachment(attachment.id, target);
  }

  List<AttachmentView> _attachments(NoteView note, AttachmentKind kind) => [
    for (final a in note.attachments)
      if (a.kind == kind) a,
    for (final a in _pendingAttachments)
      if (a.kind == kind) a,
  ];

  Widget _audioRows(NoteView note) => Column(
    mainAxisSize: MainAxisSize.min,
    children: [
      for (final (i, audio) in _attachments(note, AttachmentKind.audio).indexed)
        AttachmentAudioRow(
          audio: audio,
          canMoveEarlier: i > 0,
          canMoveLater:
              i <
              note.attachments
                      .where((a) => a.kind == AttachmentKind.audio)
                      .length -
                  1,
          transfers: _transfers,
          playing: _playingAttachment == audio.id && _playbackIsPlaying,
          canSeek: _playingAttachment == audio.id && _loadingAttachment == null,
          loading: _loadingAttachment == audio.id,
          position: _playingAttachment == audio.id
              ? _playbackPosition
              : Duration.zero,
          duration: _playingAttachment == audio.id ? _playbackDuration : null,
          onTogglePlayback: () => _togglePlayback(audio),
          onSeek: (position) => _seekPlayback(audio, position),
          playbackUnavailable: _playbackUnavailableAttachment == audio.id,
          onAction: (action) => _attachmentAction(audio, action),
          onRetry: _retryTransfers,
        ),
    ],
  );

  void _ensureAudioPlayer() {
    if (_audioPlayer != null) return;
    final player = AudioPlayer();
    _audioPlayer = player;
    _playerStateWatch = player.onPlayerStateChanged.listen((state) {
      if (!mounted) return;
      setState(() {
        _playbackIsPlaying = state == PlayerState.playing;
        if (state == PlayerState.playing) _loadingAttachment = null;
      });
    });
    _playerPositionWatch = player.onPositionChanged.listen((position) {
      if (mounted) setState(() => _playbackPosition = position);
    });
    _playerDurationWatch = player.onDurationChanged.listen((duration) {
      if (mounted) setState(() => _playbackDuration = duration);
    });
    _playerCompleteWatch = player.onPlayerComplete.listen((_) {
      if (!mounted) return;
      setState(() {
        _playingAttachment = null;
        _loadingAttachment = null;
        _playbackIsPlaying = false;
        _playbackPosition = Duration.zero;
      });
    });
  }

  Future<void> _togglePlayback(AttachmentView audio) async {
    _ensureAudioPlayer();
    final player = _audioPlayer!;
    try {
      if (_playingAttachment == audio.id && _loadingAttachment == null) {
        if (player.state == PlayerState.playing) {
          await player.pause();
        } else {
          await player.resume();
        }
        return;
      }
      setState(() {
        _loadingAttachment = audio.id;
        _playingAttachment = null;
        _playbackIsPlaying = false;
        _playbackUnavailableAttachment = null;
        _playbackPosition = Duration.zero;
        _playbackDuration = Duration(milliseconds: audio.durationMs ?? 0);
      });
      await player.stop();
      final bytes = await widget.core.readAttachment(audio.id);
      if (!mounted) return;
      if (bytes == null) {
        setState(() => _loadingAttachment = null);
        _say('This audio is still downloading. Try again when it is here.');
        return;
      }
      setState(() {
        _playingAttachment = audio.id;
        _loadingAttachment = audio.id;
        _playbackIsPlaying = false;
      });
      await player.play(BytesSource(bytes, mimeType: audio.mime));
      if (mounted && _loadingAttachment == audio.id) {
        setState(() => _loadingAttachment = null);
      }
    } on Object catch (e) {
      if (!mounted) return;
      setState(() {
        _playingAttachment = null;
        _loadingAttachment = null;
        _playbackIsPlaying = false;
        _playbackUnavailableAttachment = audio.id;
      });
      _say('Couldn\'t play this audio on this device: ${_reason(e)}');
    }
  }

  Future<void> _seekPlayback(AttachmentView audio, Duration position) async {
    if (_playingAttachment != audio.id || _audioPlayer == null) return;
    setState(() => _playbackPosition = position);
    try {
      await _audioPlayer!.seek(position);
    } on Object catch (e) {
      if (mounted) _say('Couldn\'t seek this audio: ${_reason(e)}');
    }
  }

  Future<void> _stopPlayback() async {
    final player = _audioPlayer;
    if (player == null) return;
    if (mounted) {
      setState(() {
        _playingAttachment = null;
        _loadingAttachment = null;
        _playbackIsPlaying = false;
        _playbackPosition = Duration.zero;
      });
    }
    try {
      await player.stop();
    } on Object {
      // The note may be closing while its platform player is stopping.
    }
  }

  @override
  void dispose() {
    unawaited(_viewWatch?.cancel());
    unawaited(_transferWatch?.cancel());
    unawaited(_playerStateWatch?.cancel());
    unawaited(_playerPositionWatch?.cancel());
    unawaited(_playerDurationWatch?.cancel());
    unawaited(_playerCompleteWatch?.cancel());
    if (_audioPlayer case final player?) unawaited(player.dispose());
    // A set: an item Enter made is under its stand-in ID and its own.
    for (final focus in _itemFocusNodes.values.toSet()) {
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

  /// Seed only on opening a checklist or switching to it. An empty view after
  /// someone deletes the last item must stay empty, including on stream updates.
  Future<void> _startEmptyChecklist() async {
    final note = widget.core.current;
    if (note.kind == NoteKind.checklist && note.items.isEmpty) {
      await _addListEntry();
    }
  }

  Future<void> _toggleKind() async {
    final kind = widget.core.current.kind == NoteKind.text
        ? NoteKind.checklist
        : NoteKind.text;
    await widget.core.setKind(kind);
    if (mounted && kind == NoteKind.checklist) await _startEmptyChecklist();
  }

  Future<void> _addListEntry() async {
    _itemWriteStateChanged(true);
    try {
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
      if (mounted) _focusForItem(id).requestFocus();
    } finally {
      _itemWriteStateChanged(false);
    }
  }

  /// Share: the first time, set up an account and a name, then straight on
  /// to the people on this note and the links that add more.
  Future<void> _share() async {
    if (!await ensureSharingSetUp(context, widget.invites) || !mounted) return;
    await showCollaborators(
      context,
      invites: widget.invites,
      noteId: widget.noteId,
      relayUrl: widget.relayUrl,
      relayAccess: widget.relayAccess,
    );
  }

  Future<void> _shareContent(NoteView note) => showDialog<void>(
    context: context,
    builder: (context) => _ContentShareDialog(
      text: contentForSharing(note),
      format: note.kind == NoteKind.text ? 'text' : 'checklist',
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
    fieldId: _standInFor[item.id] ?? item.id,
    focusNode: _focusForItem(item.id),
    reorderIndex: reorderIndex,
    onChecked: (v) async =>
        widget.core.setChecked(await _resolve(item.id), v ?? false),
    onTextChanged: (t) async =>
        widget.core.setItemText(await _resolve(item.id), t),
    onWriteStateChanged: _itemWriteStateChanged,
    onDelete: () async => _removeItem(await _resolve(item.id)),
    onSettled: widget.library?.sendHeldEdits,
    onBackspaceEmpty: () async {
      final id = await _resolve(item.id);
      final items = widget.core.current.items;
      final index = items.indexWhere((candidate) => candidate.id == id);
      final previous = index > 0 ? items[index - 1] : null;
      await _removeItem(id);
      if (previous != null && mounted) {
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted) _focusForItem(previous.id).requestFocus();
        });
      }
    },
    onSubmitted: onSubmitted,
  );

  Future<void> _removeItem(String id) async {
    await widget.core.removeItem(id);
    _newBlankItems.remove(id);
    // Keep the detached node until this note closes; disposing it here can
    // race the stream rebuild that unmounts its row.
    _itemRowKeys.remove(id);
  }

  /// The core's ID for a row's item, waiting for it if the row is one Enter
  /// made and the core has not answered for yet.
  Future<String> _resolve(String id) async {
    final answered = _answered[id];
    if (answered != null) return answered;
    for (final pending in _pending) {
      if (pending.key == id) return pending.id.future;
    }
    return id;
  }

  /// Enter in an item: a new one after it, with the keyboard in it.
  ///
  /// The row is drawn and focused *now*, under a stand-in ID, and the core is
  /// asked for the item afterwards. Moving the keyboard to another field
  /// retires Android's input connection, and a letter sent to the retired one
  /// is dropped. So the move has to happen at Enter, before the next letter —
  /// not a round trip to the core later, which is exactly when a person is
  /// typing it. When the core answers, the real item takes over the stand-in's
  /// row key and focus node, so the keyboard does not move again.
  Future<void> _insertItemAfter(String itemId) async {
    final pending = _PendingItem('pending:${_pendingCount++}', itemId);
    setState(() => _pending.add(pending));
    _focusForItem(pending.key).requestFocus();
    _itemWriteStateChanged(true);
    try {
      final after = await _resolve(itemId);
      final id = await widget.core.addItem('');
      _newBlankItems.add(id);
      _itemRowKeys[id] = _itemRowKeys.putIfAbsent(
        pending.key,
        () => GlobalKey<_ItemRowState>(debugLabel: 'item:$id'),
      );
      _itemFocusNodes[id] = _focusForItem(pending.key);
      _answered[pending.key] = id;
      _standInFor[id] = pending.key;
      pending.id.complete(id);
      final current = widget.core.current.items;
      final index = current.indexWhere((item) => item.id == after);
      if (index >= 0) await widget.core.moveItem(id, index + 1);
      pending.placed = true;
    } catch (e) {
      if (!pending.id.isCompleted) pending.id.completeError(e);
      if (mounted) setState(() => _pending.remove(pending));
      rethrow;
    } finally {
      _itemWriteStateChanged(false);
    }
    if (mounted) setState(() {});
  }

  /// The unchecked rows as drawn: the core's, with each pending item after the
  /// row it was made from, until the core's view has it in that place.
  List<ItemView> _withPending(NoteView note, List<ItemView> active) {
    if (_pending.isEmpty) return active;
    final ids = [for (final item in note.items) item.id];
    // A pending item is done with once the core has placed it and the view
    // shows it where it was drawn.
    _pending.removeWhere((pending) {
      final id = _answered[pending.key];
      if (!pending.placed || id == null) return false;
      final at = ids.indexOf(id);
      if (at < 0) return false;
      final after = _answered[pending.after] ?? pending.after;
      return at > 0 && ids[at - 1] == after;
    });
    final shown = {for (final p in _pending) _answered[p.key]}..remove(null);
    final rows = <ItemView>[];
    final drawn = <_PendingItem>{};
    void follow(String id) {
      for (final pending in _pending) {
        if (pending.after != id && _answered[pending.after] != id) continue;
        if (!drawn.add(pending)) continue;
        final real = _answered[pending.key];
        rows.add(
          ItemView(
            id: real ?? pending.key,
            text: '',
            checked: false,
            order: '',
          ),
        );
        follow(pending.key);
        if (real != null) follow(real);
      }
    }

    for (final item in active) {
      if (shown.contains(item.id)) continue;
      rows.add(item);
      follow(item.id);
    }
    // Made after a row that is not drawn here — Enter in a crossed-off item —
    // so it goes where the core will put a new unchecked item: last.
    for (final pending in _pending.where((p) => !drawn.contains(p)).toList()) {
      follow(pending.after);
    }
    return rows;
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
    autofocus: widget.autofocusBody && note.kind == NoteKind.text,
    onEdit: widget.core.editBody,
  );

  /// A text note: the body, and under it any items a merge left on it — drawn,
  /// because the rule is that a merge never hides anything, but not reorderable,
  /// because switching to a checklist is the way to work on them as a list.
  Widget _text(NoteView note) => LayoutBuilder(
    builder: (context, constraints) {
      // Give the body the unused viewport height so tapping its blank area
      // focuses the editor, while keeping room for any items a merge left here.
      // Sound files are rows under it too, and must not be pushed off screen.
      final audioRows = _attachments(note, AttachmentKind.audio).length;
      final bodyHeight =
          (constraints.maxHeight - note.items.length * 56 - audioRows * 72 - 16)
              .clamp(160.0, double.infinity)
              .toDouble();
      return ListView(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        children: [
          SizedBox(height: bodyHeight, child: _bodyField(note, expands: true)),
          for (final item in note.items) _row(item),
          _audioRows(note),
        ],
      );
    },
  );

  /// A checklist, with any body a merge left on it above the items.
  Widget _checklist(NoteView note) {
    final active = _withPending(
      note,
      note.items.where((item) => !item.checked).toList(),
    );
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
          ? null
          : Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: _bodyField(note),
            ),
      footer: _audioRows(note),
      itemCount: itemCount,
      // Only unchecked rows have drag handles. Convert the target position in
      // that visible group back to the core's complete item order.
      onReorderItem: (oldIndex, newIndex) {
        if (oldIndex >= active.length) return;
        final moving = active[oldIndex];
        if (_pending.isNotEmpty) return;
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
              // Same density as _ItemRow, so the label lines up with the text
              // field: compact trims the tile's horizontal inset by 4.
              dense: true,
              visualDensity: VisualDensity.compact,
              leading: const Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  SizedBox(width: 40),
                  SizedBox(width: 48, height: 48, child: Icon(Icons.add)),
                ],
              ),
              title: const Text('Checklist item'),
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
            for (final name in _preparing)
              ListTile(
                dense: true,
                leading: const SizedBox.square(
                  dimension: 20,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
                title: Text(
                  'Preparing $name',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
            if (_attachments(note, AttachmentKind.image) case final images
                when images.isNotEmpty)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
                child: AttachmentGallery(
                  images: images,
                  read: widget.core.readAttachment,
                  transfers: _transfers,
                  onOpen: (index) => _openImage(images, index),
                  onRetry: _retryTransfers,
                ),
              ),
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
        bottomNavigationBar: AnimatedPadding(
          duration: const Duration(milliseconds: 180),
          curve: Curves.easeOut,
          padding: EdgeInsets.only(
            bottom: MediaQuery.viewInsetsOf(context).bottom,
          ),
          child: library == null || summary == null
              ? _KindToggleBar(
                  onAttach: _addAttachment,
                  onSend: () => _shareContent(widget.core.current),
                  isText: isText,
                  onKindToggle: _toggleKind,
                )
              : _NoteToolbar(
                  onAttach: _addAttachment,
                  onSend: () => _shareContent(widget.core.current),
                  note: summary,
                  library: library,
                  isText: isText,
                  onKindToggle: _toggleKind,
                  onLabels: widget.onLabels,
                  onReminders: widget.onReminders,
                  onTrash: () async {
                    await library.setTrashed(summary.id, !summary.trashed);
                    if (mounted) Navigator.of(context).pop();
                  },
                ),
        ),
      ),
    );
  }
}

class _NoteToolbar extends StatelessWidget {
  const _NoteToolbar({
    required this.onAttach,
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
  final Future<void> Function() onAttach;
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
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8),
        child: Row(
          children: [
            _AttachButton(onAttach: onAttach),
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
    ),
  );
}

class _KindToggleBar extends StatelessWidget {
  const _KindToggleBar({
    required this.onAttach,
    required this.isText,
    required this.onKindToggle,
    required this.onSend,
  });

  final Future<void> Function() onAttach;
  final bool isText;
  final VoidCallback onKindToggle;
  final Future<void> Function() onSend;

  @override
  Widget build(BuildContext context) => SafeArea(
    child: Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8),
      child: Row(
        children: [
          _AttachButton(onAttach: onAttach),
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
    ),
  );
}

/// Add attachment, at the bottom left of both toolbars.
class _AttachButton extends StatelessWidget {
  const _AttachButton({required this.onAttach});

  final Future<void> Function() onAttach;

  @override
  Widget build(BuildContext context) => IconButton(
    key: const Key('add-attachment'),
    tooltip: 'Add attachment',
    icon: const Icon(Icons.add_box_outlined),
    onPressed: () => unawaited(onAttach()),
  );
}

/// The shared words for someone who cannot see a picture.
class _DescribeDialog extends StatefulWidget {
  const _DescribeDialog({required this.initial});

  final String initial;

  @override
  State<_DescribeDialog> createState() => _DescribeDialogState();
}

class _DescribeDialogState extends State<_DescribeDialog> {
  late final _text = TextEditingController(text: widget.initial);

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Describe'),
    content: TextField(
      key: const Key('attachment-description'),
      controller: _text,
      autofocus: true,
      maxLength: 1000,
      maxLines: null,
      decoration: const InputDecoration(
        helperText: 'Everyone on this note sees this description.',
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Cancel'),
      ),
      TextButton(
        onPressed: () => Navigator.of(context).pop(_text.text.trim()),
        child: const Text('Save'),
      ),
    ],
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
    this.autofocus = false,
    required this.onEdit,
  });

  final String body;
  final String hint;
  final bool expands;
  final bool autofocus;
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
  void _adopt(String body) => adoptText(_controller, body);

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
      autofocus: widget.autofocus,
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
    required this.fieldId,
    required this.focusNode,
    this.reorderIndex,
    required this.onChecked,
    required this.onTextChanged,
    required this.onWriteStateChanged,
    required this.onDelete,
    required this.onBackspaceEmpty,
    this.onSettled,
    this.onSubmitted,
  });

  final ItemView item;

  /// What the text field is keyed by: the item's ID, or the stand-in its row
  /// was first drawn under (see [_NoteScreenState._standInFor]).
  final String fieldId;
  final FocusNode focusNode;
  final int? reorderIndex;
  final ValueChanged<bool?> onChecked;
  final Future<void> Function(String) onTextChanged;
  final ValueChanged<bool> onWriteStateChanged;
  final VoidCallback onDelete;
  final Future<void> Function() onBackspaceEmpty;

  /// Typing has stopped on this row — its focus moved away — so anything held
  /// for a pause should go to the relay now.
  final VoidCallback? onSettled;
  final Future<void> Function()? onSubmitted;

  @override
  State<_ItemRow> createState() => _ItemRowState();
}

class _ItemRowState extends State<_ItemRow> {
  late final _controller = TextEditingController(text: widget.item.text);

  /// The text the core was last told this row holds, or last told this row it
  /// holds: what leaving it would send, and what the next remote view is
  /// compared against.
  late String _lastWritten = widget.item.text;

  /// Texts this row has sent whose view has not come back yet, oldest first.
  /// A view carrying one of them is this device's own typing arriving late,
  /// not the other person's, and adopting it would wind the field back.
  final List<String> _sent = [];
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
    if (!_focus.hasFocus && !_submitting) {
      unawaited(_commit());
      // Leaving a row is the end of typing on it, so what was held for a pause
      // goes now: the next thing this person does is usually the next row, and
      // the other phone should not be a keystroke behind the whole list.
      widget.onSettled?.call();
    }
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
        if (!_sent.contains(next)) _sent.add(next);
        await widget.onTextChanged(next);
        _lastWritten = next;
        next = _queuedText;
      }
    } finally {
      _writing = false;
      _writeDone = null;
      done.complete();
      widget.onWriteStateChanged(false);
    }
    // A view that came in while the queue was busy was passed over; look at
    // the latest now, in case it was someone else's.
    if (mounted) _consider(widget.item.text);
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
      // Not in turn: saving this row's text waits for its ID when it is one
      // Enter made, and the next row has to be there before the next letter.
      final saved = _commit();
      await widget.onSubmitted?.call();
      await saved;
    } finally {
      _submitting = false;
    }
  }

  @override
  void didUpdateWidget(_ItemRow old) {
    super.didUpdateWidget(old);
    _consider(widget.item.text);
  }

  /// The view's text arrived. Take it unless it is this row's own typing
  /// coming back, and keep the caret where the person left it when it is not.
  ///
  /// While a send is in flight every view is passed over: one of them may be
  /// older than what is in the field, and adopting it would wind the text back
  /// mid-word. [_write] looks again at the latest one when it finishes.
  void _consider(String text) {
    if (_writing) return;
    final own = _sent.lastIndexOf(text);
    if (own >= 0) {
      // Everything sent up to it has come back too, by now or never.
      _sent.removeRange(0, own + 1);
      _lastWritten = text;
      return;
    }
    if (text == _lastWritten) return;
    if (text == _controller.text) {
      _lastWritten = text;
      return;
    }
    // Someone else's change: it is what the core holds now, and what leaving
    // this row would send from.
    _sent.clear();
    adoptText(_controller, text);
    _lastWritten = text;
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
      dense: true,
      visualDensity: VisualDensity.compact,
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
            )
          else
            const SizedBox(width: 40),
          Checkbox(
            key: Key('check:${item.id}'),
            value: item.checked,
            onChanged: widget.onChecked,
          ),
        ],
      ),
      title: Focus(
        onKeyEvent: (node, event) {
          if (event is KeyDownEvent &&
              event.logicalKey == LogicalKeyboardKey.backspace &&
              _controller.text.isEmpty) {
            unawaited(widget.onBackspaceEmpty());
            return KeyEventResult.handled;
          }
          return KeyEventResult.ignored;
        },
        child: TextField(
          key: Key('text:${widget.fieldId}'),
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
          // Enter moves the keyboard to the item it makes, and nowhere else.
          // Flutter's default for `next` would move it at once to whatever
          // field happens to come next.
          onEditingComplete: widget.onSubmitted == null ? null : () {},
        ),
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

/// An item Enter has made, drawn before the core has answered for it.
class _PendingItem {
  _PendingItem(this.key, this.after);

  /// The stand-in ID the row is drawn under until the core's arrives.
  final String key;

  /// The item — or stand-in — it was made after.
  final String after;

  final Completer<String> id = Completer<String>();

  /// Moved into place in the core, so the core's view will show it there.
  bool placed = false;
}
