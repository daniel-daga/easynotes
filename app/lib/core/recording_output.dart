/// Temporary paths for microphone recordings, including browser Blob URLs.
library;

import 'recording_output_stub.dart'
    if (dart.library.io) 'recording_output_io.dart'
    if (dart.library.js_interop) 'recording_output_web.dart'
    as platform;

Future<String> newRecordingPath() => platform.newRecordingPath();

Future<void> discardRecordingPath(String? path) =>
    platform.discardRecordingPath(path);
