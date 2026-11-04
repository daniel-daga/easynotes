import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

/// Longer than any recording a run still holding the microphone can be, so a
/// sweep cannot take a file away from one.
const _staleRecordingAge = Duration(hours: 1);

const _recordingPrefix = 'easynotes-voice-';

/// Recordings the app kept for the person, which the sweep must never delete.
const _keptRecordingPrefix = 'easynotes-recording-';

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

/// Rename a recording the app could not import, so that it leaves the temp
/// namespace the startup sweep deletes, and return where it now is.
///
/// The person is told this path and told it stays, so it must outlive the
/// launch that told them.
Future<String> keepRecordingPath(String path) async {
  final file = File(path);
  final name = file.uri.pathSegments.last;
  if (!name.startsWith(_recordingPrefix)) return path;
  final kept = File(
    '${file.parent.path}${Platform.pathSeparator}'
    '$_keptRecordingPrefix${name.substring(_recordingPrefix.length)}',
  );
  try {
    return (await file.rename(kept.path)).path;
  } on Object catch (error) {
    // Say where it actually is, even though the sweep may take it later.
    debugPrint('could not keep the recording out of the temp sweep: $error');
    return path;
  }
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
