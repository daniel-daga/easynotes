/// How sync is shown, in one place because two screens show it.
///
/// The note list's button is the whole device — every note, worst state first —
/// and a checklist's is that one note. They are the same button because they
/// answer the same question: is what I typed anywhere but here?
library;

import 'package:flutter/material.dart';

import '../core/sync_status.dart';

/// What the sync state says, in one sentence, for a tooltip or a dialog.
///
/// Deliberately about the user's data rather than about the socket: "waiting to
/// reach the relay" is a fact they can act on, `ECONNREFUSED` is not. The error
/// itself is still there for anyone who opens the detail.
String syncSentence(SyncStatus status) {
  final waiting = switch (status.pending) {
    0 => '',
    1 => ', 1 edit waiting',
    _ => ', ${status.pending} edits waiting',
  };
  return switch (status.state) {
    SyncState.offline => 'Not syncing$waiting',
    SyncState.connecting => 'Connecting$waiting',
    SyncState.online => 'Catching up$waiting',
    SyncState.synced => status.pending == 0 ? 'Up to date' : 'Sending$waiting',
  };
}

/// The sentence, and the relay's own words underneath it if there were any.
Future<void> showSyncDetail(BuildContext context, SyncStatus status) {
  return showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: const Text('Sync'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(syncSentence(status)),
          if (status.lastError != null) ...[
            const SizedBox(height: 12),
            SelectableText(
              status.lastError!,
              key: const Key('sync-error'),
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ],
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Done'),
        ),
      ],
    ),
  );
}

/// The sync state, as one button that explains itself when tapped.
class SyncButton extends StatelessWidget {
  const SyncButton({super.key, required this.status, required this.onPressed});

  final SyncStatus status;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final icon = switch (status.state) {
      SyncState.offline => Icons.cloud_off_outlined,
      SyncState.connecting => Icons.cloud_queue_outlined,
      SyncState.online => Icons.cloud_sync_outlined,
      SyncState.synced => Icons.cloud_done_outlined,
    };
    final button = IconButton(
      key: const Key('sync-status'),
      icon: Icon(icon),
      tooltip: syncSentence(status),
      onPressed: onPressed,
    );
    if (status.pending == 0) return button;
    return Badge.count(count: status.pending, child: button);
  }
}

/// The sync state for one screen, fed by its own stream.
///
/// Its own stream on purpose: sync changes on the relay's schedule, not on the
/// note's, and rebuilding a list every time a socket reconnects would be silly.
class SyncStatusButton extends StatelessWidget {
  const SyncStatusButton({
    super.key,
    required this.stream,
    required this.initial,
  });

  final Stream<SyncStatus> stream;
  final SyncStatus initial;

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<SyncStatus>(
      stream: stream,
      initialData: initial,
      builder: (context, snapshot) {
        final status = snapshot.data ?? const SyncStatus.offline();
        return SyncButton(
          status: status,
          onPressed: () => showSyncDetail(context, status),
        );
      },
    );
  }
}
