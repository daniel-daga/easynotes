import 'dart:js_interop';
import 'dart:typed_data';

import 'package:web/web.dart' as web;

const _thumbnailEdge = 512;

/// A thumbnail made by the browser's decoder, off the Dart main thread.
///
/// `compute` is not an isolate on the web, so the image package's decode froze
/// the tab. `createImageBitmap` does the decode and resize natively, and
/// `convertToBlob` encodes the result without a round trip through Dart.
Future<Uint8List?> makeWebThumbnail(Uint8List bytes) async {
  web.ImageBitmap? bitmap;
  try {
    final blob = web.Blob(<JSAny>[bytes.toJS].toJS);
    final head = await web.window.createImageBitmap(blob).toDart;
    bitmap = head;
    final longest = head.width > head.height ? head.width : head.height;
    final scale = longest > _thumbnailEdge ? _thumbnailEdge / longest : 1.0;
    final width = (head.width * scale).round().clamp(1, _thumbnailEdge);
    final height = (head.height * scale).round().clamp(1, _thumbnailEdge);
    final canvas = web.OffscreenCanvas(width, height);
    final context =
        canvas.getContext('2d') as web.OffscreenCanvasRenderingContext2D?;
    if (context == null) return null;
    context.drawImage(head, 0, 0, width, height);
    for (final quality in [0.8, 0.6, 0.4]) {
      final out = await canvas
          .convertToBlob(
            web.ImageEncodeOptions(type: 'image/jpeg', quality: quality),
          )
          .toDart;
      final data = Uint8List.view((await out.arrayBuffer().toDart).toDart);
      if (data.lengthInBytes <= 256 << 10) return data;
    }
    return null;
  } on Object {
    return null;
  } finally {
    bitmap?.close();
  }
}
