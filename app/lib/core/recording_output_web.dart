import 'package:web/web.dart' as web;

Future<String> newRecordingPath() async =>
    'easynotes-voice-${DateTime.now().microsecondsSinceEpoch}.wav';

Future<void> discardRecordingPath(String? path) async {
  if (path == null || !path.startsWith('blob:')) return;
  web.URL.revokeObjectURL(path);
}

/// Nothing to move: a browser's recording is a blob URL, released on failure by
/// [discardRecordingPath].
Future<String> keepRecordingPath(String path) async => path;

/// A recording in a browser is a blob URL, which goes with the tab that made
/// it: there is nothing left in the temp folder for a later run to sweep.
Future<void> sweepStaleRecordings() async {}
