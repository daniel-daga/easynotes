import 'dart:js_interop';
import 'dart:typed_data';

import 'package:audioplayers/audioplayers.dart';
import 'package:web/web.dart' as web;

import 'audio_playback_types.dart';

Future<PreparedAudio> prepareAudio(Uint8List bytes, String? mime) async {
  final blob = web.Blob(
    <JSAny>[bytes.toJS].toJS,
    web.BlobPropertyBag(type: mime ?? 'application/octet-stream'),
  );
  final url = web.URL.createObjectURL(blob);
  return (
    source: UrlSource(url, mimeType: mime),
    dispose: () async => web.URL.revokeObjectURL(url),
  );
}
