/// Which sync server this collection uses, out of the ones this device has
/// seen — and the way to add another.
///
/// Switching servers is a bigger change than it looks: each server has its
/// own notes and its own account, which is why a saved one is confirmed before
/// it opens and a new one goes through [RelaySetupWizard] first.
library;

import 'package:flutter/material.dart';

import '../core/relay_profiles.dart';
import 'relay_address.dart';
import 'relay_setup_wizard.dart';

class ServerSwitcherScreen extends StatefulWidget {
  ServerSwitcherScreen({
    super.key,
    required this.profile,
    required this.profiles,
    required this.onSwitchRelay,
    required this.onVerifyRelay,
    this.onMoveLocalToRelay,
    this.hasLocalNotes = false,
    bool? servedOverHttps,
    bool? mobileRelease,
  }) : servedOverHttps = servedOverHttps ?? platformServedOverHttps,
       mobileRelease = mobileRelease ?? platformMobileRelease;

  final RelayProfile profile;
  final List<RelayProfile> profiles;

  /// Switch to the relay at a canonical URL, or to notes on this device for
  /// null, keeping [access] as that relay's access key when given.
  final Future<void> Function(String?, {String? access}) onSwitchRelay;

  /// Prove an address and key to the relay without committing anything.
  final Future<void> Function(String url, {String? access}) onVerifyRelay;

  /// Assign this local collection to a new relay, keeping its notes. Only
  /// offered when [profile] stays on this device.
  final Future<void> Function(String, {String? access})? onMoveLocalToRelay;

  /// Whether the local collection has notes to take along, so the wizard
  /// only asks the question when there is something to answer.
  final bool hasLocalNotes;

  /// A browser page served over HTTPS can only reach a relay with TLS; a
  /// released phone app likewise.
  final bool servedOverHttps;

  /// A released phone app cannot reach a relay without TLS either.
  final bool mobileRelease;

  /// Whether [url], a canonical relay URL, is one this build cannot reach.
  bool refuses(String? url) =>
      (servedOverHttps || mobileRelease) &&
      (url?.startsWith('http://') ?? false);

  @override
  State<ServerSwitcherScreen> createState() => _ServerSwitcherScreenState();
}

class _ServerSwitcherScreenState extends State<ServerSwitcherScreen> {
  bool _switching = false;

  /// Open a collection this device already knows, after saying what that does
  /// to the notes on screen. No handshake: this profile has been connected
  /// before, and its key is already kept.
  Future<void> _switchTo(RelayProfile target) async {
    if (_switching) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Switch sync server?'),
        content: Text(
          'Each sync server has separate notes and an account. Your current '
          'notes stay here, and you can switch back to see them again. '
          'Pending edits resume syncing when you select this server again. '
          'Its saved notes will open. '
          'Switch to ${target.label}?',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Switch'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() => _switching = true);
    try {
      await widget.onSwitchRelay(target.url);
    } on Object catch (error) {
      if (!mounted) return;
      setState(() => _switching = false);
      final message = error is StateError
          ? error.message.toString()
          : error.toString();
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not switch sync server: $message')),
      );
      return;
    }
    if (mounted) Navigator.of(context).pop(true);
  }

  /// A server this device has not used yet: checked before anything is kept.
  /// From a collection that is still local, the same flow asks whether its
  /// notes come along.
  Future<void> _connectAnother() async {
    if (_switching) return;
    final connected = await Navigator.of(context).push<bool>(
      MaterialPageRoute<bool>(
        builder: (_) => RelaySetupWizard(
          mode: widget.profile.url == null
              ? RelaySetupMode.connect
              : RelaySetupMode.switchServer,
          onSwitchRelay: widget.onSwitchRelay,
          onVerifyRelay: widget.onVerifyRelay,
          onMoveLocalToRelay: widget.onMoveLocalToRelay,
          profile: widget.profile,
          profiles: widget.profiles,
          hasLocalNotes: widget.hasLocalNotes,
          servedOverHttps: widget.servedOverHttps,
          mobileRelease: widget.mobileRelease,
        ),
      ),
    );
    if (connected == true && mounted) Navigator.of(context).pop(true);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final profiles = widget.profiles;
    return Scaffold(
      appBar: AppBar(title: const Text('Sync server')),
      body: ListView(
        padding: const EdgeInsets.all(24),
        children: [
          Text(
            'Each sync server has its own notes and account. Switching opens '
            'that collection; the notes here stay in this one.',
            key: const Key('server-switcher-intro'),
            style: theme.textTheme.bodyMedium?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
          const SizedBox(height: 16),
          for (final profile in profiles)
            ListTile(
              key: Key('saved-server-${profile.url ?? 'local'}'),
              contentPadding: EdgeInsets.zero,
              leading: Icon(
                profile.id == widget.profile.id
                    ? Icons.radio_button_checked
                    : Icons.radio_button_unchecked,
              ),
              title: Text(profile.label),
              subtitle: profile.id == widget.profile.id
                  ? const Text('Selected')
                  : widget.refuses(profile.url)
                  ? const Text('Needs an https:// server in this version')
                  : null,
              enabled:
                  !_switching &&
                  profile.id != widget.profile.id &&
                  !widget.refuses(profile.url),
              onTap: () => _switchTo(profile),
            ),
          const SizedBox(height: 24),
          FilledButton.icon(
            key: const Key('connect-server'),
            onPressed: _switching ? null : _connectAnother,
            icon: const Icon(Icons.add),
            label: const Text('Connect another server'),
          ),
          if (_switching) ...[
            const SizedBox(height: 16),
            const Center(child: CircularProgressIndicator()),
          ],
        ],
      ),
    );
  }
}
