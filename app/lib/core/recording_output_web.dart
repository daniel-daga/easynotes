import 'package:web/web.dart' as web;

Future<String> newRecordingPath() async =>
    'easynotes-voice-${DateTime.now().microsecondsSinceEpoch}.wav';

Future<void> discardRecordingPath(String? path) async {
  if (path == null || !path.startsWith('blob:')) return;
  web.URL.revokeObjectURL(path);
}
