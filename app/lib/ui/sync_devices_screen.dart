import 'package:flutter/material.dart';

import '../core/note_library.dart';
import '../core/relay_profiles.dart';
import '../core/sync_status.dart';
import 'account_screen.dart';
import 'account_transfer_screen.dart';
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
    this.onMoveLocalToRelay,
    this.onSetAccess,
    this.onSwitchRelay = _ignoreRelaySwitch,
  });

  final NoteLibrary library;
  final RelayProfile profile;
  final VoidCallback onServer;
  final Future<void> Function()? onAccountDeleted;
  final Future<void> Function(String, {String? access})? onMoveLocalToRelay;
  final Future<void> Function(String?)? onSetAccess;
  final Future<void> Function(String?, {String? access}) onSwitchRelay;

  static Future<void> _ignoreRelaySwitch(String? _, {String? access}) async {}

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: const Text('Sync & devices'),
      actions: [
        TextButton.icon(
          key: const Key('sync-now'),
          onPressed: profile.url == null
              ? null
              : () async {
                  try {
                    await library.syncNow();
                    if (context.mounted) {
                      ScaffoldMessenger.of(context).showSnackBar(
                        const SnackBar(content: Text('Sync is up to date.')),
                      );
                    }
                  } on Object catch (error) {
                    if (context.mounted) {
                      ScaffoldMessenger.of(context).showSnackBar(
                        SnackBar(content: Text('Sync did not finish: $error')),
                      );
                    }
                  }
                },
          icon: const Icon(Icons.sync),
          label: const Text('Sync now'),
        ),
      ],
    ),
    body: ListView(
      padding: const EdgeInsets.all(24),
      children: [
        const Text(
          'Your notes are saved on this device. Both steps are optional.',
          key: Key('setup-intro'),
        ),
        const SizedBox(height: 8),
        const Text(
          'Want backups? Connect a sync server. Your notes can be stored '
          'there without an account.',
          key: Key('relay-benefit'),
        ),
        const SizedBox(height: 8),
        const Text(
          'Want device sync and sharing? Create an account and use it with '
          'your sync server.',
          key: Key('account-benefit'),
        ),
        const Divider(height: 40),
        Text('Sync', style: Theme.of(context).textTheme.titleLarge),
        const SizedBox(height: 12),
        // Mount one subscription for the route's lifetime.
        _RelayStatus(library: library, hasRelay: profile.url != null),
        if (profile.url == null) ...[
          const SizedBox(height: 12),
          FilledButton.icon(
            key: const Key('set-up-sync'),
            onPressed: onServer,
            icon: const Icon(Icons.sync),
            label: const Text('Connect a sync server'),
          ),
        ],
        const Divider(height: 40),
        Text(
          'Access on other devices',
          style: Theme.of(context).textTheme.titleLarge,
        ),
        const SizedBox(height: 12),
        AccountScreen(
          library: library,
          profile: profile,
          onDeleted: onAccountDeleted,
          embedded: true,
        ),
        if (library is AccountTransfers && library.hasAccount) ...[
          const SizedBox(height: 8),
          ListTile(
            key: const Key('account-transfer'),
            contentPadding: EdgeInsets.zero,
            leading: const Icon(Icons.qr_code_2),
            title: const Text('Add another device'),
            subtitle: const Text(
              'Transfer account access with a one-time QR code.',
            ),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => AccountTransferScreen(
                  library: library as AccountTransfers,
                  profile: profile,
                  onMoveLocalToRelay:
                      onMoveLocalToRelay ??
                      (address, {access}) async =>
                          onSwitchRelay(address, access: access),
                  onSetAccess: onSetAccess ?? (_) async {},
                ),
              ),
            ),
          ),
        ],
        const Divider(height: 40),
        ListTile(
          key: const Key('sync-server-settings'),
          contentPadding: EdgeInsets.zero,
          leading: const Icon(Icons.dns_outlined),
          title: const Text('Sync server'),
          subtitle: Text(profile.label),
          trailing: const Icon(Icons.chevron_right),
          onTap: onServer,
        ),
        const Text(
          'Each sync server has its own collection and account. Switching '
          'servers opens that collection; your current notes stay in this one.',
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
                ? 'Your notes are saved on this device. Choose a server to '
                      'sync them, with the option to move your current notes.'
                : status.state == SyncState.synced && status.pending == 0
                ? 'Your notes’ encrypted updates have reached your server.'
                : 'Your notes stay available on this device. Changes sync '
                      'to your server when connected.',
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
