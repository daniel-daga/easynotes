import 'dart:typed_data';

import 'package:audioplayers/audioplayers.dart';

import 'audio_playback_types.dart';

/// Only reached on a platform that is neither a device nor a browser; there
/// is no temporary file to make.
Future<PreparedAudio> prepareAudio(Uint8List bytes, String? mime) async =>
    (source: BytesSource(bytes, mimeType: mime), dispose: () async {});
