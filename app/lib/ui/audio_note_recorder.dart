import 'dart:async';
import 'dart:typed_data';

import 'package:audioplayers/audioplayers.dart';
import 'package:cross_file/cross_file.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:record/record.dart';

import '../core/recording_output.dart';

class RecordedVoiceNote {
  const RecordedVoiceNote({
    required this.path,
    required this.duration,
    this.bytes,
  });

  final String path;
  final Duration duration;
  final Uint8List? bytes;
}

Future<RecordedVoiceNote?> showAudioNoteRecorder(BuildContext context) =>
    showModalBottomSheet<RecordedVoiceNote>(
      context: context,
      useSafeArea: true,
      isScrollControlled: true,
      isDismissible: false,
      enableDrag: false,
      builder: (_) => const AudioNoteRecorderSheet(),
    );

class AudioNoteRecorderSheet extends StatefulWidget {
  const AudioNoteRecorderSheet({super.key});

  @override
  State<AudioNoteRecorderSheet> createState() => _AudioNoteRecorderSheetState();
}

class _AudioNoteRecorderSheetState extends State<AudioNoteRecorderSheet>
    with WidgetsBindingObserver {
  final AudioRecorder _recorder = AudioRecorder();
  Timer? _clock;
  DateTime? _startedAt;
  String? _requestedPath;
  String? _recordedPath;
  Uint8List? _recordedBytes;
  AudioPlayer? _previewPlayer;
  StreamSubscription<void>? _previewCompleteWatch;
  bool _busy = false;
  bool _recording = false;
  bool _previewPlaying = false;
  bool _kept = false;
  String? _error;
  Duration _elapsed = Duration.zero;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed && _recording && !_busy) {
      unawaited(_stop());
    }
  }

  Future<void> _start() async {
    if (_busy || _recording) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      if (!await _recorder.hasPermission()) {
        if (mounted) {
          setState(() {
            _busy = false;
            _error = 'Microphone access was not granted.';
          });
        }
        return;
      }
      final path = await newRecordingPath();
      _requestedPath = path;
      await _recorder.start(
        const RecordConfig(encoder: AudioEncoder.wav),
        path: path,
      );
      if (!mounted) {
        await _recorder.cancel();
        await discardRecordingPath(path);
        return;
      }
      _startedAt = DateTime.now();
      _clock = Timer.periodic(const Duration(milliseconds: 250), (_) {
        final started = _startedAt;
        if (mounted && started != null) {
          setState(() => _elapsed = DateTime.now().difference(started));
        }
      });
      setState(() {
        _recording = true;
        _busy = false;
      });
    } on Object catch (error) {
      try {
        await _recorder.cancel();
      } on Object {
        // The recorder may already have stopped.
      }
      await discardRecordingPath(_requestedPath);
      _requestedPath = null;
      if (!mounted) return;
      setState(() {
        _busy = false;
        _error = 'Could not start recording: $error';
      });
    }
  }

  Future<void> _stop() async {
    if (_busy || !_recording) return;
    setState(() => _busy = true);
    _clock?.cancel();
    try {
      final path = await _recorder.stop();
      if (!mounted) return;
      final elapsed = DateTime.now().difference(_startedAt ?? DateTime.now());
      _recordedPath = path ?? _requestedPath;
      _startedAt = null;
      setState(() {
        _recording = false;
        _busy = false;
        _elapsed = elapsed;
      });
    } on Object catch (error) {
      try {
        await _recorder.cancel();
      } on Object {
        // The recorder may already have stopped.
      }
      await discardRecordingPath(_requestedPath);
      _requestedPath = null;
      _startedAt = null;
      if (!mounted) return;
      setState(() {
        _recording = false;
        _busy = false;
        _elapsed = Duration.zero;
        _error = 'Could not finish recording: $error';
      });
    }
  }

  Future<Uint8List> _bytesForPreview() async {
    final cached = _recordedBytes;
    if (cached != null) return cached;
    final path = _recordedPath;
    if (path == null) throw StateError('There is no recording to preview.');
    final bytes = await XFile(path).readAsBytes();
    _recordedBytes = bytes;
    return bytes;
  }

  Future<void> _preview() async {
    if (_busy || _recordedPath == null) return;
    final player = _previewPlayer ?? AudioPlayer();
    if (_previewPlayer == null) {
      _previewPlayer = player;
      _previewCompleteWatch = player.onPlayerComplete.listen((_) {
        if (mounted) setState(() => _previewPlaying = false);
      });
    }
    try {
      if (_previewPlaying) {
        await player.pause();
      } else {
        final source = kIsWeb
            ? BytesSource(await _bytesForPreview(), mimeType: 'audio/wav')
            : DeviceFileSource(_recordedPath!, mimeType: 'audio/wav');
        await player.play(source);
      }
      if (mounted) setState(() => _previewPlaying = !_previewPlaying);
    } on Object catch (error) {
      if (mounted) {
        setState(() => _error = 'Could not preview the recording: $error');
      }
    }
  }

  Future<void> _keep() async {
    final path = _recordedPath;
    if (_busy || path == null) return;
    setState(() => _busy = true);
    try {
      final bytes = kIsWeb ? await _bytesForPreview() : null;
      await _previewPlayer?.dispose();
      _previewPlayer = null;
      await _previewCompleteWatch?.cancel();
      _previewCompleteWatch = null;
      if (!mounted) return;
      _kept = true;
      Navigator.of(
        context,
      ).pop(RecordedVoiceNote(path: path, duration: _elapsed, bytes: bytes));
    } on Object catch (error) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = 'Could not read the recording: $error';
        });
      }
    }
  }

  Future<void> _discard() async {
    if (_busy) return;
    setState(() => _busy = true);
    _clock?.cancel();
    try {
      if (_recording) await _recorder.cancel();
      await _previewPlayer?.dispose();
      _previewPlayer = null;
      await _previewCompleteWatch?.cancel();
      _previewCompleteWatch = null;
      await discardRecordingPath(_recordedPath ?? _requestedPath);
      if (mounted) Navigator.of(context).pop();
    } on Object catch (error) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = 'Could not discard the recording: $error';
        });
      }
    }
  }

  String get _elapsedLabel {
    final seconds = _elapsed.inSeconds;
    final minutes = seconds ~/ 60;
    return '${minutes.toString().padLeft(2, '0')}:'
        '${(seconds % 60).toString().padLeft(2, '0')}';
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _clock?.cancel();
    if (!_kept) {
      if (_recording) unawaited(_recorder.cancel());
      unawaited(discardRecordingPath(_recordedPath ?? _requestedPath));
    }
    unawaited(_previewCompleteWatch?.cancel());
    unawaited(_previewPlayer?.dispose());
    unawaited(_recorder.dispose());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final ready = _recordedPath != null;
    return PopScope(
      canPop: false,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(24, 20, 24, 28),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text('Audio note', style: Theme.of(context).textTheme.titleLarge),
            const SizedBox(height: 12),
            Text(
              _recording
                  ? 'Recording from the microphone. Audio stays on this device until you save it.'
                  : ready
                  ? 'Listen back, then keep or discard this recording.'
                  : 'Your microphone is used only while recording. The audio is attached to an encrypted note when you keep it.',
              textAlign: TextAlign.center,
            ),
            const SizedBox(height: 20),
            if (_recording) ...[
              Text(
                _elapsedLabel,
                style: Theme.of(context).textTheme.displaySmall,
              ),
              const SizedBox(height: 16),
              FilledButton.icon(
                onPressed: _busy ? null : _stop,
                icon: const Icon(Icons.stop),
                label: const Text('Stop recording'),
              ),
            ] else if (ready) ...[
              Text(
                _elapsedLabel,
                style: Theme.of(context).textTheme.displaySmall,
              ),
              const SizedBox(height: 12),
              OutlinedButton.icon(
                onPressed: _busy ? null : _preview,
                icon: Icon(_previewPlaying ? Icons.pause : Icons.play_arrow),
                label: Text(_previewPlaying ? 'Pause preview' : 'Play preview'),
              ),
              const SizedBox(height: 8),
              FilledButton.icon(
                onPressed: _busy ? null : _keep,
                icon: const Icon(Icons.check),
                label: const Text('Keep recording'),
              ),
              TextButton(
                onPressed: _busy ? null : _discard,
                child: const Text('Discard'),
              ),
            ] else ...[
              FilledButton.icon(
                onPressed: _busy ? null : _start,
                icon: _busy
                    ? const SizedBox.square(
                        dimension: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.mic),
                label: Text(
                  _busy ? 'Requesting microphone…' : 'Start recording',
                ),
              ),
              TextButton(
                onPressed: _busy ? null : _discard,
                child: const Text('Cancel'),
              ),
            ],
            if (_error != null) ...[
              const SizedBox(height: 8),
              Text(
                _error!,
                textAlign: TextAlign.center,
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
