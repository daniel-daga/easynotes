/// A note's photos and sound files, as the editor draws them
/// ([plan](../../../docs/plan-attachments.md), "User interface").
///
/// Display only. Where the bytes come from, whether an upload is waiting and
/// what removing one means are the screen's and the core's; these widgets are
/// handed a way to read bytes, the transfers that concern them, and callbacks.
library;

import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';

import '../core/attachments.dart';

/// Reads an attachment's thumbnail or whole file, or null while it is not on
/// this device; see `NoteCore.readAttachment`.
typedef AttachmentReader =
    Future<Uint8List?> Function(String id, {bool thumbnail});

/// The tallest a gallery gets, whatever it holds.
const _galleryHeight = 220.0;

/// The pictures above a note's title: one bounded preview, or a grid of up to
/// four with the rest counted on the last tile.
class AttachmentGallery extends StatelessWidget {
  const AttachmentGallery({
    super.key,
    required this.images,
    required this.read,
    required this.transfers,
    required this.onOpen,
    required this.onRetry,
  });

  final List<AttachmentView> images;
  final AttachmentReader read;
  final List<AttachmentTransfer> transfers;
  final ValueChanged<int> onOpen;
  final VoidCallback onRetry;

  static const _shown = 4;

  @override
  Widget build(BuildContext context) {
    if (images.isEmpty) return const SizedBox.shrink();
    Widget tile(int index, {int more = 0}) => _GalleryTile(
      key: ValueKey('attachment:${images[index].id}'),
      image: images[index],
      read: read,
      transfers: transfers,
      more: more,
      onTap: () => onOpen(index),
      onRetry: onRetry,
    );
    if (images.length == 1) {
      return SizedBox(height: _galleryHeight, child: tile(0));
    }
    final shown = images.length > _shown ? _shown : images.length;
    final more = images.length - shown;
    // Two to a row: two images side by side, four as two rows of two, three
    // as a row of two over one.
    final rows = <Widget>[];
    for (var start = 0; start < shown; start += 2) {
      rows.add(
        Expanded(
          child: Row(
            children: [
              for (var i = start; i < start + 2 && i < shown; i++)
                Expanded(
                  child: Padding(
                    padding: EdgeInsets.only(
                      left: i == start ? 0 : 2,
                      right: i == start && i + 1 < shown ? 2 : 0,
                    ),
                    child: tile(i, more: i == shown - 1 ? more : 0),
                  ),
                ),
            ],
          ),
        ),
      );
      if (start + 2 < shown) rows.add(const SizedBox(height: 4));
    }
    return SizedBox(
      height: shown > 2 ? _galleryHeight : _galleryHeight * 0.66,
      child: Column(children: rows),
    );
  }
}

class _GalleryTile extends StatelessWidget {
  const _GalleryTile({
    super.key,
    required this.image,
    required this.read,
    required this.transfers,
    required this.more,
    required this.onTap,
    required this.onRetry,
  });

  final AttachmentView image;
  final AttachmentReader read;
  final List<AttachmentTransfer> transfers;
  final int more;
  final VoidCallback onTap;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final status = attachmentStatus(image, transfers);
    final label = image.description.isNotEmpty ? image.description : image.name;
    return Semantics(
      button: true,
      label: more > 0 ? '$label, and $more more' : label,
      child: ClipRRect(
        borderRadius: BorderRadius.circular(12),
        child: Material(
          color: theme.colorScheme.surfaceContainerHighest,
          child: InkWell(
            onTap: onTap,
            child: Stack(
              fit: StackFit.expand,
              children: [
                AttachmentImage(
                  id: image.id,
                  read: read,
                  // A preview when there is one; the original, which is here
                  // for an import of this device's, when there is not.
                  thumbnail: image.hasThumbnail,
                  fit: BoxFit.cover,
                  revision: Object.hashAll(transfers),
                ),
                if (more > 0)
                  ColoredBox(
                    color: theme.colorScheme.scrim.withValues(alpha: 0.45),
                    child: Center(
                      child: Text(
                        '+$more',
                        style: theme.textTheme.headlineSmall?.copyWith(
                          color: Colors.white,
                        ),
                      ),
                    ),
                  ),
                if (status != null)
                  Align(
                    alignment: Alignment.bottomLeft,
                    child: Padding(
                      padding: const EdgeInsets.all(6),
                      child: _StatusChip(status: status, onRetry: onRetry),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// A picture's bytes, read when it is built and read again whenever
/// [revision] changes while they are still missing: that is a transfer
/// having moved, and they may have arrived.
class AttachmentImage extends StatefulWidget {
  const AttachmentImage({
    super.key,
    required this.id,
    required this.read,
    required this.thumbnail,
    this.fit = BoxFit.contain,
    this.revision = 0,
  });

  final String id;
  final AttachmentReader read;
  final bool thumbnail;
  final BoxFit fit;
  final int revision;

  @override
  State<AttachmentImage> createState() => _AttachmentImageState();
}

class _AttachmentImageState extends State<AttachmentImage> {
  Uint8List? _bytes;
  bool _failed = false;
  int _asked = 0;

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  @override
  void didUpdateWidget(AttachmentImage old) {
    super.didUpdateWidget(old);
    if (old.id != widget.id || old.thumbnail != widget.thumbnail) {
      _bytes = null;
      _failed = false;
      unawaited(_load());
    } else if (_bytes == null && old.revision != widget.revision) {
      unawaited(_load());
    }
  }

  Future<void> _load() async {
    final asked = ++_asked;
    try {
      final bytes = await widget.read(widget.id, thumbnail: widget.thumbnail);
      if (!mounted || asked != _asked) return;
      setState(() => _bytes = bytes);
    } on Object {
      if (!mounted || asked != _asked) return;
      setState(() => _failed = true);
    }
  }

  @override
  Widget build(BuildContext context) {
    final bytes = _bytes;
    final colour = Theme.of(context).colorScheme.onSurfaceVariant;
    if (bytes == null) {
      return Center(
        child: Icon(
          _failed ? Icons.broken_image_outlined : Icons.image_outlined,
          color: colour,
        ),
      );
    }
    return Image.memory(
      bytes,
      fit: widget.fit,
      gaplessPlayback: true,
      excludeFromSemantics: true,
      errorBuilder: (_, _, _) =>
          Center(child: Icon(Icons.broken_image_outlined, color: colour)),
    );
  }
}

/// What a person is told about an attachment that is not simply there.
@immutable
class AttachmentStatus {
  const AttachmentStatus(this.text, {this.retry = false, this.progress});

  final String text;

  /// Whether a Retry action belongs beside it.
  final bool retry;

  /// 0 to 1, while chunks are moving.
  final double? progress;
}

/// The status of [attachment] from what the transfer queue says, or null when
/// there is nothing to say: a published attachment that is not moving.
AttachmentStatus? attachmentStatus(
  AttachmentView attachment,
  List<AttachmentTransfer> transfers,
) {
  AttachmentTransfer? find(bool upload) => transfers
      .where((t) => t.attachment == attachment.id && t.upload == upload)
      .firstOrNull;
  if (attachment.pending) {
    final up = find(true);
    return switch (up?.state) {
      null ||
      TransferState.waiting => const AttachmentStatus('Waiting to upload'),
      TransferState.running => AttachmentStatus(
        'Uploading',
        progress: up!.of == 0 ? null : up.done / up.of,
      ),
      TransferState.retrying => const AttachmentStatus(
        'Couldn\'t upload. Trying again…',
      ),
      TransferState.failed => const AttachmentStatus(
        'Couldn\'t upload',
        retry: true,
      ),
    };
  }
  final down = find(false);
  return switch (down?.state) {
    null => null,
    TransferState.waiting => const AttachmentStatus('Connect to download'),
    TransferState.running => AttachmentStatus(
      'Downloading',
      progress: down!.of == 0 ? null : down.done / down.of,
    ),
    TransferState.retrying => const AttachmentStatus(
      'Couldn\'t download. Trying again…',
    ),
    TransferState.failed => const AttachmentStatus(
      'Couldn\'t download',
      retry: true,
    ),
  };
}

class _StatusChip extends StatelessWidget {
  const _StatusChip({required this.status, required this.onRetry});

  final AttachmentStatus status;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Material(
      color: theme.colorScheme.surface.withValues(alpha: 0.9),
      shape: const StadiumBorder(),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (status.progress != null) ...[
              SizedBox(
                width: 12,
                height: 12,
                child: CircularProgressIndicator(
                  value: status.progress,
                  strokeWidth: 2,
                ),
              ),
              const SizedBox(width: 6),
            ],
            Text(status.text, style: theme.textTheme.labelSmall),
            if (status.retry)
              TextButton(
                style: TextButton.styleFrom(
                  visualDensity: VisualDensity.compact,
                  tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                ),
                onPressed: onRetry,
                child: const Text('Retry'),
              ),
          ],
        ),
      ),
    );
  }
}

/// What an attachment's menu can do.
enum AttachmentAction { saveCopy, describe, moveEarlier, moveLater, remove }

/// One sound file below the note's content, with controls for its local copy.
class AttachmentAudioRow extends StatelessWidget {
  const AttachmentAudioRow({
    super.key,
    required this.audio,
    required this.transfers,
    required this.onAction,
    required this.onRetry,
    required this.playing,
    required this.canSeek,
    required this.loading,
    required this.position,
    required this.duration,
    required this.onTogglePlayback,
    required this.onSeek,
    this.playbackUnavailable = false,
    this.canMoveEarlier = false,
    this.canMoveLater = false,
  });

  final AttachmentView audio;
  final bool canMoveEarlier;
  final bool canMoveLater;
  final List<AttachmentTransfer> transfers;
  final ValueChanged<AttachmentAction> onAction;
  final VoidCallback onRetry;
  final bool playing;
  final bool canSeek;
  final bool loading;
  final Duration position;
  final Duration? duration;
  final VoidCallback onTogglePlayback;
  final ValueChanged<Duration> onSeek;
  final bool playbackUnavailable;

  @override
  Widget build(BuildContext context) {
    final status = attachmentStatus(audio, transfers);
    final facts = [
      if (audio.durationMs != null) formatDuration(audio.durationMs!),
      formatSize(audio.size),
    ].join(' · ');
    final total = duration ?? Duration(milliseconds: audio.durationMs ?? 0);
    final maxMs = total.inMilliseconds;
    final valueMs = position.inMilliseconds.clamp(0, maxMs).toInt();
    return Column(
      key: ValueKey('attachment:${audio.id}'),
      children: [
        ListTile(
          leading: IconButton(
            tooltip: loading
                ? 'Loading audio'
                : playing
                ? 'Pause audio'
                : 'Play audio',
            onPressed: loading ? null : onTogglePlayback,
            icon: loading
                ? const SizedBox.square(
                    dimension: 22,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : Icon(
                    playing
                        ? Icons.pause_circle_filled
                        : Icons.play_circle_filled,
                  ),
          ),
          title: Semantics(
            label: audio.name,
            excludeSemantics: true,
            child: Text(
              audio.name,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          subtitle: Text(
            playbackUnavailable
                ? 'Playback unavailable on this device · $facts'
                : status == null
                ? facts
                : '$facts · ${status.text}',
          ),
          trailing: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (status?.retry ?? false)
                TextButton(onPressed: onRetry, child: const Text('Retry')),
              AttachmentMenu(
                pending: audio.pending,
                onAction: onAction,
                describe: false,
                canMoveEarlier: canMoveEarlier,
                canMoveLater: canMoveLater,
              ),
            ],
          ),
        ),
        if (maxMs > 0)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: Row(
              children: [
                Text(
                  formatDuration(valueMs),
                  style: Theme.of(context).textTheme.labelSmall,
                ),
                Expanded(
                  child: Slider(
                    key: ValueKey('audio-seek:${audio.id}'),
                    min: 0,
                    max: maxMs.toDouble(),
                    value: valueMs.toDouble(),
                    onChanged: canSeek
                        ? (value) =>
                              onSeek(Duration(milliseconds: value.round()))
                        : null,
                  ),
                ),
                Text(
                  formatDuration(maxMs),
                  style: Theme.of(context).textTheme.labelSmall,
                ),
              ],
            ),
          ),
      ],
    );
  }
}

/// The overflow menu an attachment carries, in the viewer and on a sound
/// file's row.
class AttachmentMenu extends StatelessWidget {
  const AttachmentMenu({
    super.key,
    required this.pending,
    required this.onAction,
    this.describe = true,
    this.canMoveEarlier = false,
    this.canMoveLater = false,
  });

  final bool pending;
  final bool describe;
  final bool canMoveEarlier;
  final bool canMoveLater;
  final ValueChanged<AttachmentAction> onAction;

  @override
  Widget build(BuildContext context) => PopupMenuButton<AttachmentAction>(
    key: const Key('attachment-menu'),
    tooltip: 'Attachment actions',
    icon: const Icon(Icons.more_vert),
    onSelected: onAction,
    itemBuilder: (context) => [
      const PopupMenuItem(
        value: AttachmentAction.saveCopy,
        child: ListTile(
          leading: Icon(Icons.download_outlined),
          title: Text('Save a copy'),
        ),
      ),
      // A description is shared, so it waits until the attachment is.
      if (describe && !pending)
        const PopupMenuItem(
          value: AttachmentAction.describe,
          child: ListTile(
            leading: Icon(Icons.short_text),
            title: Text('Describe'),
          ),
        ),
      // Order is shared too, and a pending import has no place in it yet.
      if (!pending && canMoveEarlier)
        const PopupMenuItem(
          value: AttachmentAction.moveEarlier,
          child: ListTile(
            leading: Icon(Icons.arrow_upward),
            title: Text('Move earlier'),
          ),
        ),
      if (!pending && canMoveLater)
        const PopupMenuItem(
          value: AttachmentAction.moveLater,
          child: ListTile(
            leading: Icon(Icons.arrow_downward),
            title: Text('Move later'),
          ),
        ),
      const PopupMenuItem(
        value: AttachmentAction.remove,
        child: ListTile(
          leading: Icon(Icons.delete_outline),
          title: Text('Remove attachment'),
        ),
      ),
    ],
  );
}

/// Pictures one at a time, whole: swipe or use the arrows between them, pinch
/// or scroll to zoom. It follows [images] rather than keeping a copy, so a
/// move, a description or a removal — from here or from someone else — shows
/// at once, and the picture on screen stays the one that was being looked at.
class AttachmentViewer extends StatefulWidget {
  const AttachmentViewer({
    super.key,
    required this.images,
    required this.initialImages,
    required this.initial,
    required this.read,
    required this.onAction,
  });

  final Stream<List<AttachmentView>> images;
  final List<AttachmentView> initialImages;
  final int initial;
  final AttachmentReader read;

  /// Carried out by the screen that opened this. Removing pops the viewer.
  final Future<void> Function(AttachmentView, AttachmentAction) onAction;

  @override
  State<AttachmentViewer> createState() => _AttachmentViewerState();
}

class _AttachmentViewerState extends State<AttachmentViewer> {
  late final _pages = PageController(initialPage: widget.initial);
  late String _current = widget.initialImages[widget.initial].id;

  @override
  void dispose() {
    _pages.dispose();
    super.dispose();
  }

  void _go(int index) => _pages.animateToPage(
    index,
    duration: const Duration(milliseconds: 200),
    curve: Curves.easeOut,
  );

  @override
  Widget build(BuildContext context) => StreamBuilder<List<AttachmentView>>(
    stream: widget.images,
    initialData: widget.initialImages,
    builder: (context, snapshot) {
      final images = snapshot.data ?? widget.initialImages;
      final index = images.indexWhere((a) => a.id == _current);
      if (index < 0) {
        // Removed, here or elsewhere: nothing left to show.
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted) Navigator.of(context).maybePop();
        });
        return const Scaffold();
      }
      // A move put the picture somewhere else: follow it.
      if (_pages.hasClients && _pages.page?.round() != index) {
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted && _pages.hasClients) _pages.jumpToPage(index);
        });
      }
      final current = images[index];
      return Scaffold(
        appBar: AppBar(
          title: Text(
            images.length == 1
                ? current.name
                : '${index + 1} of ${images.length}',
            overflow: TextOverflow.ellipsis,
          ),
          actions: [
            IconButton(
              key: const Key('attachment-save'),
              tooltip: 'Save a copy',
              icon: const Icon(Icons.download_outlined),
              onPressed: () =>
                  widget.onAction(current, AttachmentAction.saveCopy),
            ),
            AttachmentMenu(
              pending: current.pending,
              canMoveEarlier: index > 0 && !images[index - 1].pending,
              canMoveLater:
                  index < images.length - 1 && !images[index + 1].pending,
              onAction: (action) => widget.onAction(current, action),
            ),
          ],
        ),
        body: Stack(
          children: [
            PageView.builder(
              controller: _pages,
              itemCount: images.length,
              onPageChanged: (page) =>
                  setState(() => _current = images[page].id),
              itemBuilder: (context, page) {
                final image = images[page];
                return Semantics(
                  image: true,
                  label: image.description.isNotEmpty
                      ? image.description
                      : image.name,
                  child: InteractiveViewer(
                    maxScale: 8,
                    child: SizedBox.expand(
                      child: AttachmentImage(
                        key: ValueKey('viewer:${image.id}'),
                        id: image.id,
                        read: widget.read,
                        thumbnail: false,
                      ),
                    ),
                  ),
                );
              },
            ),
            if (index > 0)
              Align(
                alignment: Alignment.centerLeft,
                child: IconButton(
                  tooltip: 'Previous image',
                  icon: const Icon(Icons.chevron_left),
                  onPressed: () => _go(index - 1),
                ),
              ),
            if (index < images.length - 1)
              Align(
                alignment: Alignment.centerRight,
                child: IconButton(
                  tooltip: 'Next image',
                  icon: const Icon(Icons.chevron_right),
                  onPressed: () => _go(index + 1),
                ),
              ),
          ],
        ),
      );
    },
  );
}

String formatSize(int bytes) {
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).round()} KB';
  return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
}

String formatDuration(int ms) {
  final seconds = ms ~/ 1000;
  final m = seconds ~/ 60;
  final s = (seconds % 60).toString().padLeft(2, '0');
  return '$m:$s';
}

/// Where the cards below it get their cover pictures: a way to read a note's
/// cover thumbnail, and a revision that moves whenever a transfer does, so a
/// card waiting for its picture asks again.
class CoverScope extends StatefulWidget {
  const CoverScope({
    super.key,
    required this.read,
    required this.transfers,
    required this.child,
  });

  /// A note's cover thumbnail, or null while it is not here.
  final Future<Uint8List?> Function(String noteId) read;
  final Stream<List<AttachmentTransfer>> transfers;
  final Widget child;

  static CoverScopeData? of(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<CoverScopeData>();

  @override
  State<CoverScope> createState() => _CoverScopeState();
}

class _CoverScopeState extends State<CoverScope> {
  StreamSubscription<List<AttachmentTransfer>>? _watch;
  int _revision = 0;

  @override
  void initState() {
    super.initState();
    _watch = widget.transfers.listen((transfers) {
      if (mounted) setState(() => _revision = Object.hashAll(transfers));
    });
  }

  @override
  void dispose() {
    unawaited(_watch?.cancel());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => CoverScopeData(
    read: widget.read,
    revision: _revision,
    child: widget.child,
  );
}

/// What a [CoverScope] hands the cards under it.
class CoverScopeData extends InheritedWidget {
  const CoverScopeData({
    super.key,
    required this.read,
    required this.revision,
    required super.child,
  });

  final Future<Uint8List?> Function(String noteId) read;
  final int revision;

  @override
  bool updateShouldNotify(CoverScopeData old) =>
      old.revision != revision || old.read != read;
}

/// A note's cover above its card's title, cropped to a band so a portrait
/// photo does not make one card a screen tall. Nothing without a
/// [CoverScope] above it.
class NoteCover extends StatelessWidget {
  const NoteCover({
    super.key,
    required this.noteId,
    required this.attachment,
    this.width,
    this.height,
  });

  final String noteId;
  final String attachment;
  final int? width;
  final int? height;

  @override
  Widget build(BuildContext context) {
    final scope = CoverScope.of(context);
    if (scope == null) return const SizedBox.shrink();
    final w = width, h = height;
    final ratio = w == null || h == null || h == 0
        ? 4 / 3
        : (w / h).clamp(1.0, 2.0);
    return AspectRatio(
      aspectRatio: ratio,
      child: ColoredBox(
        color: Theme.of(context).colorScheme.surfaceContainerHighest,
        child: AttachmentImage(
          // Keyed on the attachment, so a new cover is a new picture rather
          // than the old one left on screen.
          key: ValueKey('cover:$noteId:$attachment'),
          id: noteId,
          read: (id, {thumbnail = true}) => scope.read(id),
          thumbnail: true,
          fit: BoxFit.cover,
          revision: scope.revision,
        ),
      ),
    );
  }
}
