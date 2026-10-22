/// Settings for sync: whether it is working, the two setup steps if they are
/// still open, the server and its access key, and the account that follows.
///
/// The same two-step story as the first run, told with the same cards, and
/// then only the controls that are still useful: a person who is fully set up
/// sees one line saying so, not a checklist of things they already did.
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show Clipboard, ClipboardData;

import '../core/note_library.dart';
import '../core/relay_profiles.dart';
import '../core/sync_status.dart';
import 'account_screen.dart';
import 'account_transfer_screen.dart';
import 'last_synced_footer.dart';
import 'relay_setup_wizard.dart';
import 'server_switcher_screen.dart';
import 'setup_widgets.dart';
import 'sync_button.dart';

/// Relay delivery and account access answer different questions. Keep them
/// together without treating an account as a prerequisite for uploading notes.
class SyncDevicesScreen extends StatefulWidget {
  const SyncDevicesScreen({
    super.key,
    required this.library,
    required this.profile,
    this.profiles = const [],
    this.onAccountDeleted,
    this.onMoveLocalToRelay,
    this.onSetAccess,
    this.onSwitchRelay = _ignoreRelaySwitch,
  });

  final NoteLibrary library;
  final RelayProfile profile;

  /// Every profile this device knows, so that a first connection only offers
  /// to move notes to a server that does not already hold some.
  final List<RelayProfile> profiles;

  final Future<void> Function()? onAccountDeleted;
  final Future<void> Function(String, {String? access})? onMoveLocalToRelay;
  final Future<void> Function(String?)? onSetAccess;
  final Future<void> Function(String?, {String? access}) onSwitchRelay;

  static Future<void> _ignoreRelaySwitch(String? _, {String? access}) async {}

  @override
  State<SyncDevicesScreen> createState() => _SyncDevicesScreenState();
}

class _SyncDevicesScreenState extends State<SyncDevicesScreen> {
  late RelayProfile _profile = widget.profile;
  late Stream<SyncStatus> _sync = widget.library.watchSync();

  @override
  void didUpdateWidget(SyncDevicesScreen old) {
    super.didUpdateWidget(old);
    // A different collection is a different screen, even when it reuses this
    // one's place in the tree.
    if (old.profile.id != widget.profile.id ||
        old.profile.url != widget.profile.url) {
      _profile = widget.profile;
    }
    if (!identical(old.library, widget.library)) {
      _sync = widget.library.watchSync();
    }
  }

  bool get _connected => _profile.url != null;

  /// Whether this device belongs to an account. A profile switching away can
  /// close its library while this route is still leaving; it has nothing to
  /// say about an account then.
  bool get _hasAccount {
    try {
      return widget.library.hasAccount;
    } on Object {
      return false;
    }
  }

  bool get _fullySetUp => _connected && _hasAccount;

  String get _host {
    final url = _profile.url;
    if (url == null) return 'On this device';
    final host = Uri.tryParse(url)?.host;
    return host == null || host.isEmpty ? url : host;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Sync & devices')),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 640),
          child: ListView(
            padding: const EdgeInsets.fromLTRB(24, 16, 24, 32),
            children: [
              _StatusCard(
                library: widget.library,
                stream: _sync,
                connected: _connected,
                host: _host,
                onSyncNow: _syncNow,
              ),
              if (_fullySetUp)
                _FullySetUpLine(host: _host)
              else ...[
                _SectionTitle('Set up', top: 20),
                SetupStep(
                  key: const Key('sync-setup-server-step'),
                  number: 1,
                  icon: Icons.cloud_outlined,
                  title: 'Sync server',
                  subtitle: _connected
                      ? 'Connected to $_host.'
                      : 'Back up your notes online.',
                  done: _connected,
                  enabled: !_connected,
                  onTap: _connectServer,
                ),
                SetupConnector(
                  color: _connected
                      ? theme.colorScheme.primary
                      : theme.colorScheme.outlineVariant,
                ),
                SetupStep(
                  key: const Key('sync-setup-account-step'),
                  number: 2,
                  icon: Icons.key,
                  title: 'Account',
                  subtitle: _hasAccount
                      ? 'Account created.'
                      : _connected
                      ? 'Sync between your devices and share notes.'
                      : 'Works once a sync server is connected.',
                  done: _hasAccount,
                  enabled: !_hasAccount,
                  onTap: _createAccount,
                ),
              ],
              _SectionTitle('Sync server'),
              ListTile(
                key: const Key('switch-server'),
                contentPadding: EdgeInsets.zero,
                leading: const Icon(Icons.dns_outlined),
                title: const Text('Switch server'),
                subtitle: Text(_host),
                trailing: const Icon(Icons.chevron_right),
                onTap: _switchServer,
              ),
              if (_connected) ...[
                ListTile(
                  key: const Key('relay-access-change'),
                  contentPadding: EdgeInsets.zero,
                  enabled: widget.onSetAccess != null,
                  leading: Icon(
                    _profile.access == null ? Icons.lock_open : Icons.key,
                  ),
                  title: Text(
                    _profile.access == null
                        ? 'No access key'
                        : 'Access key saved',
                  ),
                  subtitle: Text(
                    _profile.access == null
                        ? 'Add the key whoever runs the server gave you.'
                        : 'Checked before it is saved.',
                  ),
                  trailing: const Icon(Icons.chevron_right),
                  onTap: _changeAccess,
                ),
                if (_profile.relayLink != null)
                  ListTile(
                    key: const Key('relay-link-copy'),
                    contentPadding: EdgeInsets.zero,
                    leading: const Icon(Icons.link),
                    title: const Text('Copy server link'),
                    subtitle: const Text('For people you share lists with.'),
                    onTap: () => _copyRelayLink(_profile.relayLink!),
                  ),
              ],
              if (_hasAccount) ...[
                _SectionTitle('Account & devices'),
                ListTile(
                  key: const Key('account-settings'),
                  contentPadding: EdgeInsets.zero,
                  leading: const Icon(Icons.person_outline),
                  title: const Text('Account'),
                  subtitle: Text(_accountSummary),
                  trailing: const Icon(Icons.chevron_right),
                  onTap: _openAccount,
                ),
                if (widget.library is AccountTransfers)
                  ListTile(
                    key: const Key('account-transfer'),
                    contentPadding: EdgeInsets.zero,
                    leading: const Icon(Icons.qr_code_2),
                    title: const Text('Add another device'),
                    subtitle: const Text(
                      'Transfer account access with a one-time QR code.',
                    ),
                    trailing: const Icon(Icons.chevron_right),
                    onTap: _openTransfer,
                  ),
              ],
              const SizedBox(height: 16),
              ListTile(
                key: const Key('how-sync-works'),
                contentPadding: EdgeInsets.zero,
                leading: const Icon(Icons.help_outline),
                title: const Text('How sync works'),
                trailing: const Icon(Icons.chevron_right),
                onTap: _showHowSyncWorks,
              ),
            ],
          ),
        ),
      ),
    );
  }

  String get _accountSummary {
    final String? name;
    try {
      name = widget.library.profileName;
    } on Object {
      return 'This device belongs to your account.';
    }
    return name == null || name.isEmpty
        ? 'This device belongs to your account.'
        : name;
  }

  Future<void> _syncNow() async {
    try {
      await widget.library.syncNow();
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(const SnackBar(content: Text('Sync is up to date.')));
      }
    } on Object catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text('Sync did not finish: $error')));
      }
    }
  }

  /// Leave once the open collection is no longer the one this screen was
  /// drawn for. The note list underneath has been replaced by then, and this
  /// route still holds the profile that was closed.
  void _leaveIfChanged(Object? result) {
    if (result == true && mounted) {
      Navigator.of(context).popUntil((route) => route.isFirst);
    }
  }

  Future<void> _connectServer() async {
    final connected = await Navigator.of(context).push<bool>(
      MaterialPageRoute<bool>(
        builder: (_) => RelaySetupWizard(
          mode: RelaySetupMode.connect,
          onSwitchRelay: widget.onSwitchRelay,
          onVerifyRelay: widget.library.verifyRelay,
          onMoveLocalToRelay: widget.onMoveLocalToRelay,
          profile: _profile,
          profiles: widget.profiles,
        ),
      ),
    );
    _leaveIfChanged(connected);
  }

  Future<void> _switchServer() async {
    final switched = await Navigator.of(context).push<bool>(
      MaterialPageRoute<bool>(
        builder: (_) => ServerSwitcherScreen(
          profile: _profile,
          profiles: widget.profiles,
          onSwitchRelay: widget.onSwitchRelay,
          onVerifyRelay: widget.library.verifyRelay,
          onMoveLocalToRelay: widget.onMoveLocalToRelay,
        ),
      ),
    );
    _leaveIfChanged(switched);
  }

  /// Check a new key with the server, then keep it. This is the same relay and
  /// the same notes, so the screen stays where it is.
  Future<void> _changeAccess() async {
    final save = widget.onSetAccess;
    if (save == null) return;
    String? saved = _profile.access;
    var changed = false;
    final result = await Navigator.of(context).push<bool>(
      MaterialPageRoute<bool>(
        builder: (_) => RelaySetupWizard(
          mode: RelaySetupMode.accessKey,
          onSwitchRelay: widget.onSwitchRelay,
          onVerifyRelay: widget.library.verifyRelay,
          onSetAccess: (access) async {
            await save(access);
            saved = access;
            changed = true;
          },
          profile: _profile,
        ),
      ),
    );
    if (result == true && changed && mounted) {
      setState(() => _profile = _profile.withAccess(saved));
    }
  }

  Future<void> _createAccount() async {
    await Navigator.of(context).push<void>(
      MaterialPageRoute<void>(
        builder: (_) => AccountScreen(
          library: widget.library,
          profile: _profile,
          creating: true,
        ),
      ),
    );
    if (mounted) setState(() {});
  }

  Future<void> _openAccount() async {
    await Navigator.of(context).push<void>(
      MaterialPageRoute<void>(
        builder: (_) => AccountScreen(
          library: widget.library,
          profile: _profile,
          onDeleted: widget.onAccountDeleted,
        ),
      ),
    );
    if (mounted) setState(() {});
  }

  Future<void> _openTransfer() async {
    final library = widget.library;
    if (library is! AccountTransfers) return;
    final transfers = library as AccountTransfers;
    await Navigator.of(context).push<void>(
      MaterialPageRoute<void>(
        builder: (_) => AccountTransferScreen(
          library: transfers,
          profile: _profile,
          onMoveLocalToRelay:
              widget.onMoveLocalToRelay ??
              (address, {access}) =>
                  widget.onSwitchRelay(address, access: access),
          onSetAccess: widget.onSetAccess ?? (_) async {},
        ),
      ),
    );
    if (mounted) setState(() {});
  }

  Future<void> _copyRelayLink(String link) async {
    await Clipboard.setData(ClipboardData(text: link));
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(
        content: Text(
          'Server link copied. It lets whoever has it use this server, so '
          'send it only to people you share lists with.',
        ),
      ),
    );
  }

  /// The three sentences the first run opens with, kept for whenever someone
  /// wonders what any of this is for.
  Future<void> _showHowSyncWorks() => showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (context) => SafeArea(
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(24, 0, 24, 32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              'How sync works',
              style: Theme.of(context).textTheme.titleLarge,
            ),
            const SizedBox(height: 20),
            const FeatureRow(
              icon: Icons.phone_android,
              title: 'On this device',
              detail:
                  'Your notes are saved here, and nobody needs an account '
                  'to write them.',
            ),
            const SizedBox(height: 20),
            const FeatureRow(
              icon: Icons.cloud_outlined,
              title: 'Sync server',
              detail:
                  'Backs up your notes online. Use one from your organization '
                  'or a sync server provider.',
            ),
            const SizedBox(height: 20),
            const FeatureRow(
              icon: Icons.key,
              title: 'Account',
              detail:
                  'Syncs your notes between your devices and lets you share '
                  'them. It needs a sync server.',
            ),
          ],
        ),
      ),
    ),
  );
}

/// A section's name, with the air above it and the little it needs below:
/// every section keeps the same rhythm rather than each one placing its own
/// gap.
class _SectionTitle extends StatelessWidget {
  const _SectionTitle(this.title, {this.top = 32});

  final String title;

  /// The space between the last row of the section before and this name.
  final double top;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: EdgeInsets.only(top: top, bottom: 4),
      child: Text(
        title,
        style: theme.textTheme.titleSmall?.copyWith(
          color: theme.colorScheme.primary,
          fontWeight: FontWeight.w600,
        ),
      ),
    );
  }
}

/// The whole of "is sync working": one line, one host, and the relay's own
/// words only when there is something wrong.
class _StatusCard extends StatelessWidget {
  const _StatusCard({
    required this.library,
    required this.stream,
    required this.connected,
    required this.host,
    required this.onSyncNow,
  });

  final NoteLibrary library;
  final Stream<SyncStatus> stream;
  final bool connected;
  final String host;
  final Future<void> Function() onSyncNow;

  @override
  Widget build(BuildContext context) => StreamBuilder<SyncStatus>(
    stream: stream,
    initialData: library.syncStatus,
    builder: (context, snapshot) {
      final status = snapshot.data ?? const SyncStatus.offline();
      final scheme = Theme.of(context).colorScheme;
      final failed = connected && status.lastError != null;
      final color = !connected
          ? scheme.onSurfaceVariant
          : failed
          ? scheme.error
          : status.isConnected
          ? scheme.primary
          : scheme.onSurfaceVariant;
      return Card(
        margin: EdgeInsets.zero,
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              Icon(
                connected
                    ? failed
                          ? Icons.cloud_off_outlined
                          : _syncIcon(status)
                    : Icons.phonelink_off,
                size: 32,
                color: color,
              ),
              const SizedBox(width: 16),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      connected ? syncSentence(status) : 'Sync not set up',
                      key: const Key('relay-delivery-status'),
                      style: Theme.of(context).textTheme.titleMedium?.copyWith(
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const SizedBox(height: 2),
                    Text(
                      connected ? host : 'Your notes are saved on this device.',
                      style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                    if (failed) ...[
                      const SizedBox(height: 8),
                      SelectableText(
                        status.lastError!,
                        key: const Key('sync-error'),
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    ],
                    const SizedBox(height: 4),
                    Row(
                      children: [
                        Expanded(
                          child: LastSyncedFooter(
                            hasRelay: connected,
                            initial: library.lastSyncedAt,
                            watch: library.watchLastSyncedAt,
                            padding: const EdgeInsets.only(top: 6),
                            textAlign: TextAlign.start,
                          ),
                        ),
                        TextButton.icon(
                          key: const Key('sync-now'),
                          onPressed: connected ? onSyncNow : null,
                          icon: const Icon(Icons.sync, size: 18),
                          label: const Text('Sync now'),
                          style: TextButton.styleFrom(
                            padding: const EdgeInsets.symmetric(horizontal: 8),
                            minimumSize: const Size(0, 36),
                            tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                          ),
                        ),
                      ],
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      );
    },
  );
}

/// The one line a finished setup leaves behind.
class _FullySetUpLine extends StatelessWidget {
  const _FullySetUpLine({required this.host});

  final String host;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.only(top: 20),
      child: ListTile(
        key: const Key('sync-fully-set-up'),
        contentPadding: EdgeInsets.zero,
        minVerticalPadding: 0,
        leading: Icon(Icons.verified, color: scheme.primary, size: 22),
        title: const Text('Fully set up'),
        subtitle: Text('Backed up to $host, with an account for your devices.'),
      ),
    );
  }
}

IconData _syncIcon(SyncStatus status) => switch (status.state) {
  SyncState.empty => Icons.cloud_outlined,
  SyncState.offline => Icons.cloud_off_outlined,
  SyncState.connecting => Icons.cloud_queue_outlined,
  SyncState.online => Icons.cloud_sync_outlined,
  SyncState.synced => Icons.cloud_done_outlined,
  SyncState.paused => Icons.pause_circle_outline,
};
