import 'dart:io';

import 'package:path_provider/path_provider.dart';

Future<String> newRecordingPath() async {
  final directory = await getTemporaryDirectory();
  return '${directory.path}${Platform.pathSeparator}'
      'easynotes-voice-${DateTime.now().microsecondsSinceEpoch}.wav';
}

Future<void> discardRecordingPath(String? path) async {
  if (path == null || path.isEmpty) return;
  final file = File(path);
  if (await file.exists()) await file.delete();
}
