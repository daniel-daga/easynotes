/// How sync status is described to the person using the app.
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
    // No note has ever been made here, so there is nothing a relay could be
    // behind on. Saying "Not syncing" here reads as a fault and is not one.
    SyncState.empty => 'Nothing to sync yet',
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

/// A quiet, descriptive sync entry for the home drawer.
class SyncStatusTile extends StatelessWidget {
  const SyncStatusTile({
    super.key,
    required this.stream,
    required this.initial,
  });

  final Stream<SyncStatus> stream;
  final SyncStatus initial;

  @override
  Widget build(BuildContext context) => StreamBuilder<SyncStatus>(
    stream: stream,
    initialData: initial,
    builder: (context, snapshot) {
      final status = snapshot.data ?? const SyncStatus.offline();
      return ListTile(
        key: const Key('sync-status'),
        leading: Icon(_syncIcon(status)),
        title: const Text('Sync'),
        subtitle: Text(syncSentence(status)),
        onTap: () => showSyncDetail(context, status),
      );
    },
  );
}

IconData _syncIcon(SyncStatus status) => switch (status.state) {
  SyncState.empty => Icons.cloud_outlined,
  SyncState.offline => Icons.cloud_off_outlined,
  SyncState.connecting => Icons.cloud_queue_outlined,
  SyncState.online => Icons.cloud_sync_outlined,
  SyncState.synced => Icons.cloud_done_outlined,
};
