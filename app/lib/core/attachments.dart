/// Photos and sound files on a note, as the UI sees them
/// ([ADR 0027](../../../docs/adr/0027-attachments.md)).
///
/// Plain values, like [NoteView]. An attachment's bytes never live here: a
/// screen asks [NoteCore.readAttachment] for them when it is about to draw or
/// play one, and they arrive already decrypted and checked.
library;

import 'package:flutter/foundation.dart';

/// Whether an attachment is drawn or played.
enum AttachmentKind { image, audio }

@immutable
class AttachmentView {
  const AttachmentView({
    required this.id,
    required this.kind,
    required this.name,
    required this.mime,
    required this.size,
    this.width,
    this.height,
    this.durationMs,
    this.description = '',
    this.thumbnailWidth,
    this.thumbnailHeight,
    this.pending = false,
  });

  final String id;
  final AttachmentKind kind;

  /// The person's file name: shown, searched and used for Save a copy.
  final String name;

  /// `image/jpeg`, `audio/mp4` and so on, read from the bytes.
  final String mime;

  /// The original's size in bytes.
  final int size;
  final int? width;
  final int? height;
  final int? durationMs;

  /// Shared words for someone who cannot see or hear it.
  final String description;
  final int? thumbnailWidth;
  final int? thumbnailHeight;

  /// Saved on this device and not in the note yet — waiting to upload, or
  /// uploading. Nobody else can see it.
  final bool pending;

  bool get hasThumbnail => thumbnailWidth != null;

  @override
  bool operator ==(Object other) =>
      other is AttachmentView &&
      other.id == id &&
      other.kind == kind &&
      other.name == name &&
      other.mime == mime &&
      other.size == size &&
      other.width == width &&
      other.height == height &&
      other.durationMs == durationMs &&
      other.description == description &&
      other.thumbnailWidth == thumbnailWidth &&
      other.thumbnailHeight == thumbnailHeight &&
      other.pending == pending;

  @override
  int get hashCode => Object.hash(
    id,
    kind,
    name,
    mime,
    size,
    width,
    height,
    durationMs,
    description,
    thumbnailWidth,
    thumbnailHeight,
    pending,
  );

  @override
  String toString() => 'AttachmentView($id, "$name", $mime)';
}

/// Where one transfer is.
enum TransferState {
  /// Saved here and waiting for its note to catch up with the relay — or, for
  /// a download, for a connection.
  waiting,

  /// [AttachmentTransfer.done] of [AttachmentTransfer.of] chunks moved.
  running,

  /// Failed for a reason the network explains; it will be tried again.
  retrying,

  /// Refused, until [NoteLibrary.retryTransfers]. An upload's local copy is
  /// kept.
  failed,
}

/// One attachment transfer that has not finished.
@immutable
class AttachmentTransfer {
  const AttachmentTransfer({
    required this.note,
    required this.attachment,
    required this.upload,
    required this.thumbnail,
    required this.state,
    this.done = 0,
    this.of = 0,
    this.error,
  });

  final String note;
  final String attachment;

  /// An import going to the relay, rather than a download.
  final bool upload;

  /// For a download: the thumbnail rather than the original.
  final bool thumbnail;
  final TransferState state;
  final int done;
  final int of;

  /// The network's or the relay's words, for [TransferState.retrying] and
  /// [TransferState.failed].
  final String? error;

  @override
  bool operator ==(Object other) =>
      other is AttachmentTransfer &&
      other.note == note &&
      other.attachment == attachment &&
      other.upload == upload &&
      other.thumbnail == thumbnail &&
      other.state == state &&
      other.done == done &&
      other.of == of &&
      other.error == error;

  @override
  int get hashCode =>
      Object.hash(note, attachment, upload, thumbnail, state, done, of, error);

  @override
  String toString() =>
      'AttachmentTransfer($attachment, ${upload ? 'up' : 'down'}, $state $done/$of)';
}
