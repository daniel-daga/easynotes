/// A quiet line at the bottom of a screen saying when this device last had
/// everything the relay knows.
///
/// The app's clock rather than the core's ([NoteLibrary.lastSyncedAt]): the
/// engine measures time with an ordinal because it compiles for a browser, and
/// a footer is the one thing here that needs a `DateTime`.
///
/// Drawn only when there is a relay to have synced with, and only once this
/// session has actually synced. Hiding it before then is the honest choice: a
/// time from an earlier launch would be a promise this session has not kept.
library;

import 'dart:async';

import 'package:flutter/material.dart';

class LastSyncedFooter extends StatefulWidget {
  const LastSyncedFooter({
    super.key,
    required this.hasRelay,
    required this.initial,
    required this.watch,
  });

  /// Whether a relay is configured at all. Without one there is nothing this
  /// device could have synced.
  final bool hasRelay;

  /// [NoteLibrary.lastSyncedAt], for the first frame.
  final DateTime? initial;

  /// [NoteLibrary.watchLastSyncedAt], opened when this mounts.
  final Stream<DateTime?> Function() watch;

  @override
  State<LastSyncedFooter> createState() => _LastSyncedFooterState();
}

class _LastSyncedFooterState extends State<LastSyncedFooter> {
  late DateTime? _at = widget.initial;
  StreamSubscription<DateTime?>? _subscription;

  /// The relative wording ages on its own, so the line is redrawn while it is
  /// on screen. A minute is the smallest thing it says.
  Timer? _tick;

  @override
  void initState() {
    super.initState();
    _listen();
    _tick = Timer.periodic(const Duration(minutes: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  void _listen() {
    _subscription = widget.watch().listen((at) {
      if (!mounted || at == _at) return;
      setState(() => _at = at);
    });
  }

  @override
  void didUpdateWidget(LastSyncedFooter old) {
    super.didUpdateWidget(old);
    if (widget.watch != old.watch) {
      unawaited(_subscription?.cancel());
      _listen();
    }
  }

  @override
  void dispose() {
    unawaited(_subscription?.cancel());
    _tick?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final at = _at;
    if (!widget.hasRelay || at == null) return const SizedBox.shrink();
    final theme = Theme.of(context);
    return Padding(
      key: const Key('last-synced'),
      padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
      child: Text(
        lastSyncedLabel(at, DateTime.now()),
        textAlign: TextAlign.center,
        style: theme.textTheme.bodySmall?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

/// [at] as a footer line: relative while it is recent, and the clock time once
/// it is three hours old.
///
/// Relative reassurance first ("just now", "5m ago"), because that is what a
/// person checking a sync wants to know; an exact time once the answer is old
/// enough that "4h 20m ago" is harder to place than "3:42 PM".
String lastSyncedLabel(DateTime at, DateTime now) {
  final elapsed = now.difference(at);
  if (elapsed.inMinutes < 1) return 'Last synced just now';
  if (elapsed.inHours < 3) {
    if (elapsed.inMinutes < 60) {
      return 'Last synced ${elapsed.inMinutes}m ago';
    }
    final hours = elapsed.inHours;
    final minutes = elapsed.inMinutes % 60;
    return minutes == 0
        ? 'Last synced ${hours}h ago'
        : 'Last synced ${hours}h ${minutes}m ago';
  }
  return 'Last synced at ${_when(at, now)}';
}

/// The clock time, with the day when it is not today: a footer read after
/// midnight should not quietly claim the afternoon before.
String _when(DateTime at, DateTime now) {
  final today = DateTime(now.year, now.month, now.day);
  final day = DateTime(at.year, at.month, at.day);
  final days = today.difference(day).inDays;
  final clock = _clock(at);
  if (days <= 0) return clock;
  if (days == 1) return '$clock yesterday';
  return '$clock on ${at.month}/${at.day}';
}

String _clock(DateTime at) {
  final hour = at.hour % 12 == 0 ? 12 : at.hour % 12;
  final minute = at.minute.toString().padLeft(2, '0');
  return '$hour:$minute ${at.hour < 12 ? 'AM' : 'PM'}';
}
