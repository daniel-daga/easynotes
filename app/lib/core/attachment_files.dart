/// Where attachments come from and where a copy goes: the platform's own
/// pickers and save dialogs, behind an interface so that a screen test can
/// hand the editor files without a dialog in the way.
///
/// The bytes of an image are always read here, because a thumbnail has to be
/// made from them ([makeThumbnail]). A sound file on a device is handed over
/// by path instead, so a 50 MB recording is read a chunk at a time by the core
/// rather than copied whole across the bridge; a browser has no paths, and
/// hands over bytes.
library;

import 'package:file_picker/file_picker.dart';
import 'package:flutter/foundation.dart';
import 'package:image/image.dart' as img;

/// One file a person chose.
@immutable
class PickedFile {
  const PickedFile({required this.name, this.bytes, this.path});

  final String name;

  /// The whole file, when it was read.
  final Uint8List? bytes;

  /// Where it is on this device's disk, when it was not.
  final String? path;
}

abstract interface class AttachmentFiles {
  /// Images the person chose, in the order they chose them; empty if they
  /// cancelled.
  Future<List<PickedFile>> pickImages();

  /// Sound files, likewise.
  Future<List<PickedFile>> pickAudio();

  /// Offer [bytes] to be saved as [name]. False if the person cancelled.
  Future<bool> save(String name, String mime, Uint8List bytes);

  /// A preview of [image] for the gallery, or null if it cannot be made:
  /// [makeThumbnail], for anything but a test.
  Future<Uint8List?> thumbnail(Uint8List image);
}

/// The platform's pickers, through `file_picker`.
class DeviceAttachmentFiles implements AttachmentFiles {
  const DeviceAttachmentFiles();

  @override
  Future<List<PickedFile>> pickImages() async => [
    for (final file in await FilePicker.pickFiles(type: FileType.image))
      PickedFile(name: file.name, bytes: await file.readAsBytes()),
  ];

  @override
  Future<List<PickedFile>> pickAudio() async => [
    for (final file in await FilePicker.pickFiles(type: FileType.audio))
      kIsWeb || file.path == null
          ? PickedFile(name: file.name, bytes: await file.readAsBytes())
          : PickedFile(name: file.name, path: file.path),
  ];

  @override
  Future<bool> save(String name, String mime, Uint8List bytes) async =>
      await FilePicker.saveFile(fileName: name, bytes: bytes, mimeType: mime) !=
      null;

  @override
  Future<Uint8List?> thumbnail(Uint8List image) => makeThumbnail(image);
}

/// The longest side of a thumbnail, as the core holds it.
const thumbnailEdge = 512;

/// The largest thumbnail the core accepts.
const _thumbnailBytes = 256 << 10;

/// A preview of an image, made here because the core has no decoder: at most
/// [thumbnailEdge] pixels a side, turned the way the camera meant, with the
/// original's metadata — which may say where it was taken — left behind.
///
/// Null for anything this cannot decode. The image is still attached, just
/// without a preview; the gallery then shows the original.
Future<Uint8List?> makeThumbnail(Uint8List image) => compute(_thumbnail, image);

Uint8List? _thumbnail(Uint8List bytes) {
  try {
    final decoded = img.decodeImage(bytes);
    if (decoded == null) return null;
    final upright = img.bakeOrientation(decoded);
    final longest = upright.width > upright.height
        ? upright.width
        : upright.height;
    final scaled = longest <= thumbnailEdge
        ? upright
        : upright.width >= upright.height
        ? img.copyResize(
            upright,
            width: thumbnailEdge,
            interpolation: img.Interpolation.average,
          )
        : img.copyResize(
            upright,
            height: thumbnailEdge,
            interpolation: img.Interpolation.average,
          );
    scaled.exif = img.ExifData();
    for (final quality in [80, 60, 40]) {
      final jpeg = img.encodeJpg(scaled, quality: quality);
      if (jpeg.length <= _thumbnailBytes) return jpeg;
    }
    return null;
  } on Object {
    return null;
  }
}
