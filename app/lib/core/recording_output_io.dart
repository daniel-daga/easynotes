import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

/// Longer than any recording a run still holding the microphone can be, so a
/// sweep cannot take a file away from one.
const _staleRecordingAge = Duration(hours: 1);

const _recordingPrefix = 'easynotes-voice-';

Future<String> newRecordingPath() async {
  final directory = await getTemporaryDirectory();
  return '${directory.path}${Platform.pathSeparator}'
      '$_recordingPrefix${DateTime.now().microsecondsSinceEpoch}.wav';
}

Future<void> discardRecordingPath(String? path) async {
  if (path == null || path.isEmpty) return;
  final file = File(path);
  if (await file.exists()) await file.delete();
}

/// Delete recordings that a run which died between starting and importing left
/// in the temp folder. They are plain WAVs, so nothing else will ever remove
/// them.
Future<void> sweepStaleRecordings() async {
  try {
    await sweepStaleRecordingsIn(await getTemporaryDirectory());
  } on Object catch (error) {
    // A temp folder that cannot be read is no reason to fail a launch.
    debugPrint('could not sweep temporary recordings: $error');
  }
}

/// [sweepStaleRecordings] over a folder it is given, so that a test needs no
/// platform channel.
@visibleForTesting
Future<void> sweepStaleRecordingsIn(Directory directory) async {
  if (!await directory.exists()) return;
  final cutoff = DateTime.now().subtract(_staleRecordingAge);
  await for (final entry in directory.list(followLinks: false)) {
    try {
      if (entry is! File) continue;
      if (!entry.uri.pathSegments.last.startsWith(_recordingPrefix)) continue;
      if ((await entry.lastModified()).isBefore(cutoff)) await entry.delete();
    } on Object {
      // Gone or held by another run since it was listed: nothing to sweep.
    }
  }
}
