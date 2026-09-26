import 'package:flutter/material.dart';

import '../core/relay_profiles.dart';

class RelaySettingsScreen extends StatefulWidget {
  const RelaySettingsScreen({
    super.key,
    required this.profile,
    required this.profiles,
    required this.onSwitchRelay,
  });

  final RelayProfile profile;
  final List<RelayProfile> profiles;
  final Future<void> Function(String?) onSwitchRelay;

  @override
  State<RelaySettingsScreen> createState() => _RelaySettingsScreenState();
}

class _RelaySettingsScreenState extends State<RelaySettingsScreen> {
  final _url = TextEditingController();
  final _form = GlobalKey<FormState>();
  bool _switching = false;

  @override
  void dispose() {
    _url.dispose();
    super.dispose();
  }

  Future<void> _switchTo(String? url) async {
    if (_switching || url == widget.profile.url) return;
    final target = url ?? 'On this device';
    final destinationExists = widget.profiles.any((p) => p.url == url);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Switch relay?'),
        content: Text(
          'Each relay has separate notes and an account. Your current notes '
          'stay here, and you can switch back to see them again. '
          'Pending edits resume syncing when you select this relay again. '
          '${destinationExists ? 'Its saved notes will open. ' : 'This profile starts empty. '}'
          'Switch to $target?',
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
      await widget.onSwitchRelay(url);
      if (mounted) Navigator.of(context).pop();
    } on Object catch (error) {
      if (!mounted) return;
      setState(() => _switching = false);
      final message = error is StateError
          ? error.message.toString()
          : error.toString();
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text('Could not switch relay: $message')));
    }
  }

  String? _validate(String? value) {
    try {
      canonicalRelayUrl(value ?? '');
      return null;
    } on FormatException catch (error) {
      return error.message;
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Relay settings')),
    body: ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text('Current profile', style: Theme.of(context).textTheme.titleMedium),
        ListTile(
          contentPadding: EdgeInsets.zero,
          leading: const Icon(Icons.check_circle_outline),
          title: Text(widget.profile.label),
          subtitle: const Text('Only this profile syncs right now.'),
        ),
        const Divider(height: 32),
        Text(
          'Switch to a relay',
          style: Theme.of(context).textTheme.titleMedium,
        ),
        Form(
          key: _form,
          child: Column(
            children: [
              TextFormField(
                key: const Key('relay-url-field'),
                controller: _url,
                enabled: !_switching,
                keyboardType: TextInputType.url,
                autocorrect: false,
                decoration: const InputDecoration(
                  labelText: 'Relay URL',
                  hintText: 'https://relay.example',
                  helperText:
                      'Use the same relay as the people you share notes with.',
                ),
                validator: _validate,
                onFieldSubmitted: (_) => _submitUrl(),
              ),
              const SizedBox(height: 12),
              Align(
                alignment: Alignment.centerRight,
                child: FilledButton(
                  key: const Key('switch-relay-button'),
                  onPressed: _switching ? null : _submitUrl,
                  child: const Text('Switch relay'),
                ),
              ),
            ],
          ),
        ),
        const Divider(height: 32),
        Text('Saved profiles', style: Theme.of(context).textTheme.titleMedium),
        for (final profile in widget.profiles)
          ListTile(
            leading: Icon(
              profile.url == widget.profile.url
                  ? Icons.radio_button_checked
                  : Icons.radio_button_unchecked,
            ),
            title: Text(profile.label),
            subtitle: profile.url == widget.profile.url
                ? const Text('Selected')
                : null,
            enabled: !_switching && profile.id != widget.profile.id,
            onTap: () => _switchTo(profile.url),
          ),
        if (_switching) const Center(child: CircularProgressIndicator()),
      ],
    ),
  );

  Future<void> _submitUrl() async {
    if (!(_form.currentState?.validate() ?? false)) return;
    try {
      await _switchTo(canonicalRelayUrl(_url.text));
    } on FormatException catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error.message)));
    }
  }
}
