Future<String> newRecordingPath() async => throw UnsupportedError(
  'Audio recording is not supported on this platform.',
);

Future<void> discardRecordingPath(String? path) async {}

Future<String> keepRecordingPath(String path) async => path;

Future<void> sweepStaleRecordings() async {}
