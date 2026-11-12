import 'dart:io';
import 'dart:typed_data';

import 'package:audioplayers/audioplayers.dart';

import 'audio_playback_types.dart';

Future<PreparedAudio> prepareAudio(Uint8List bytes, String? mime) async {
  final directory = await Directory.systemTemp.createTemp('easynotes-audio-');
  final file = File('${directory.path}${Platform.pathSeparator}audio');
  await file.writeAsBytes(bytes, flush: true);
  return (
    source: DeviceFileSource(file.path, mimeType: mime),
    dispose: () async {
      try {
        await directory.delete(recursive: true);
      } on Object {
        // The player may still hold it; the temp folder sweeps it later.
      }
    },
  );
}
