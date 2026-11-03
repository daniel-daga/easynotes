/// What happens to a voice recording that no run stayed alive to import.
///
/// A recording is a plain WAV in the temp folder, so one left by a killed app
/// must not outlive its usefulness. The sweep is handed a folder here, so that
/// no platform channel and no real temp directory are involved.
library;

import 'dart:io';

import 'package:easynotes/core/recording_output_io.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late Directory directory;

  setUp(
    () => directory = Directory.systemTemp.createTempSync('easynotes-sweep-'),
  );
  tearDown(() => directory.deleteSync(recursive: true));

  File file(String name, {required Duration age}) =>
      File('${directory.path}${Platform.pathSeparator}$name')
        ..writeAsBytesSync(List.filled(16, 0))
        ..setLastModifiedSync(DateTime.now().subtract(age));

  test('a recording older than an hour is deleted', () async {
    final old = file('easynotes-voice-1.wav', age: const Duration(hours: 2));

    await sweepStaleRecordingsIn(directory);

    expect(old.existsSync(), isFalse);
  });

  test('a recording made a moment ago is kept', () async {
    final recent = file(
      'easynotes-voice-2.wav',
      age: const Duration(minutes: 5),
    );

    await sweepStaleRecordingsIn(directory);

    expect(recent.existsSync(), isTrue);
  });

  test('a file that is not a recording is kept', () async {
    final other = file('notes.txt', age: const Duration(days: 2));

    await sweepStaleRecordingsIn(directory);

    expect(other.existsSync(), isTrue);
  });

  test('a recording kept after a failed import leaves the swept name, so the '
      'next launch keeps it', () async {
    final temp = file('easynotes-voice-3.wav', age: const Duration(minutes: 5));

    final kept = await keepRecordingPath(temp.path);

    expect(temp.existsSync(), isFalse);
    expect(File(kept).existsSync(), isTrue);
    expect(
      kept.split(Platform.pathSeparator).last,
      startsWith('easynotes-recording-'),
    );
    // As old as anything the sweep would take, and still not taken.
    File(
      kept,
    ).setLastModifiedSync(DateTime.now().subtract(const Duration(hours: 2)));
    await sweepStaleRecordingsIn(directory);
    expect(File(kept).existsSync(), isTrue);
  });

  test('a folder that is not there does not throw', () async {
    final gone = Directory(
      '${directory.path}${Platform.pathSeparator}not-here',
    );

    await sweepStaleRecordingsIn(gone);
    expect(gone.existsSync(), isFalse);
  });
}
