import 'package:flutter/material.dart';

import '../core/note_library.dart';
import '../core/relay_profiles.dart';
import '../core/sync_status.dart';
import 'account_screen.dart';
import 'sync_button.dart';

/// Relay delivery and account access answer different questions. Keep them
/// together without treating an account as a prerequisite for uploading notes.
class SyncDevicesScreen extends StatelessWidget {
  const SyncDevicesScreen({
    super.key,
    required this.library,
    required this.profile,
    required this.onServer,
    this.onAccountDeleted,
  });

  final NoteLibrary library;
  final RelayProfile profile;
  final VoidCallback onServer;
  final Future<void> Function()? onAccountDeleted;

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Sync & devices')),
    body: ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text('Relay sync', style: Theme.of(context).textTheme.titleLarge),
        const SizedBox(height: 12),
        // Mount one subscription for the route's lifetime.
        _RelayStatus(library: library, hasRelay: profile.url != null),
        if (profile.url == null) ...[
          const SizedBox(height: 12),
          FilledButton.icon(
            key: const Key('set-up-sync'),
            onPressed: onServer,
            icon: const Icon(Icons.sync),
            label: const Text('Set up sync'),
          ),
        ],
        const Divider(height: 40),
        Text('Access on other devices', style: Theme.of(context).textTheme.titleLarge),
        const SizedBox(height: 12),
        AccountScreen(
          library: library,
          profile: profile,
          onDeleted: onAccountDeleted,
          embedded: true,
        ),
        const Divider(height: 40),
        ListTile(
          key: const Key('sync-server-settings'),
          contentPadding: EdgeInsets.zero,
          leading: const Icon(Icons.dns_outlined),
          title: const Text('Server (relay)'),
          subtitle: Text(profile.label),
          trailing: const Icon(Icons.chevron_right),
          onTap: onServer,
        ),
        const Text(
          'Each relay has its own collection and account. Switching relays '
          'opens that collection; your current notes stay in this one.',
        ),
      ],
    ),
  );
}

class _RelayStatus extends StatefulWidget {
  const _RelayStatus({required this.library, required this.hasRelay});
  final NoteLibrary library;
  final bool hasRelay;

  @override
  State<_RelayStatus> createState() => _RelayStatusState();
}

class _RelayStatusState extends State<_RelayStatus> {
  late final _stream = widget.library.watchSync();

  @override
  Widget build(BuildContext context) => StreamBuilder<SyncStatus>(
    stream: _stream,
    initialData: widget.library.syncStatus,
    builder: (context, snapshot) {
      final status = snapshot.data ?? const SyncStatus.offline();
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            widget.hasRelay ? syncSentence(status) : 'Sync not set up',
            key: const Key('relay-delivery-status'),
            style: Theme.of(context).textTheme.titleMedium,
          ),
          const SizedBox(height: 8),
          Text(
            !widget.hasRelay
                ? 'Your notes are saved on this device. Choose a relay to '
                    'sync them, with the option to move your current notes.'
                : status.state == SyncState.synced && status.pending == 0
                ? 'Your notes’ encrypted updates have reached your relay.'
                : 'Your notes stay available on this device. Changes sync '
                    'to your relay when connected.',
          ),
          if (widget.hasRelay && status.lastError != null) ...[
            const SizedBox(height: 12),
            SelectableText(status.lastError!, key: const Key('sync-error')),
          ],
        ],
      );
    },
  );
}
