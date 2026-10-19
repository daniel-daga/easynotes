import 'package:flutter/foundation.dart'
    show TargetPlatform, defaultTargetPlatform, kIsWeb, kReleaseMode;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show Clipboard, ClipboardData;

import '../core/relay_profiles.dart';

/// Whether the page this app was served from is HTTPS, in which case a browser
/// will refuse it a plain `http://` or `ws://` relay (mixed content).
bool get _servedOverHttps => kIsWeb && Uri.base.scheme == 'https';

/// Whether this is a release build for a phone, whose operating system refuses
/// plain `http://` and `ws://` outright: Android since API 28 unless the
/// manifest allows it (only the debug one does), iOS through App Transport
/// Security. Debug builds keep plain http for a relay on the local network.
bool get _mobileRelease =>
    kReleaseMode &&
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS);

class RelaySettingsScreen extends StatefulWidget {
  RelaySettingsScreen({
    super.key,
    required this.profile,
    required this.profiles,
    required this.onSwitchRelay,
    this.onMoveLocalToRelay,
    this.onSetAccess,
    bool? servedOverHttps,
    bool? mobileRelease,
  }) : servedOverHttps = servedOverHttps ?? _servedOverHttps,
       mobileRelease = mobileRelease ?? _mobileRelease;

  final RelayProfile profile;
  final List<RelayProfile> profiles;

  /// Switch to the relay at a canonical URL, or to notes on this device for
  /// null, keeping [access] as that relay's access key when given.
  final Future<void> Function(String?, {String? access}) onSwitchRelay;

  /// Assign the current local collection to a new relay, keeping all its
  /// notes and account. Never offered for a destination that already exists.
  final Future<void> Function(String, {String? access})? onMoveLocalToRelay;

  /// Keep a new access key for the relay in use, or forget it for null, and
  /// reconnect with it ([ADR 0028](../../../docs/adr/0028-who-may-write-to-a-relay.md)).
  /// Null where there is nothing to reconnect, as in some tests.
  final Future<void> Function(String?)? onSetAccess;

  /// A browser page served over HTTPS cannot open a connection to a relay
  /// without TLS, so such an address is explained rather than accepted. A
  /// device app has no such rule, and a relay on the local network is how it
  /// is developed.
  final bool servedOverHttps;

  /// A released phone app cannot reach a relay without TLS either, and a relay
  /// anyone else uses should not be reached without it, so the same address is
  /// refused here with its own reason.
  final bool mobileRelease;

  /// Whether [url], a canonical relay URL, is one this build cannot reach.
  bool refuses(String? url) =>
      (servedOverHttps || mobileRelease) &&
      (url?.startsWith('http://') ?? false);

  @override
  State<RelaySettingsScreen> createState() => _RelaySettingsScreenState();
}

class _RelaySettingsScreenState extends State<RelaySettingsScreen> {
  final _url = TextEditingController();
  final _access = TextEditingController();
  final _form = GlobalKey<FormState>();
  bool _switching = false;

  @override
  void dispose() {
    _url.dispose();
    _access.dispose();
    super.dispose();
  }

  Future<void> _switchTo(String? url, {String? access}) async {
    if (_switching) return;
    if (url == widget.profile.url) {
      // The relay already in use, given a key: that is entering its key.
      if (access != null && url != null) await _saveAccess(access);
      return;
    }
    final target = url ?? 'On this device';
    final destinationExists = widget.profiles.any((p) => p.url == url);
    final canMove = widget.profile.url == null &&
        url != null && !destinationExists && widget.onMoveLocalToRelay != null;
    if (canMove) {
      final move = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: const Text('Connect relay'),
          content: Text(
            'Use $target for this collection? Move your current notes, '
            'including attachments, labels and reminders, or keep them in '
            'On this device and start with an empty collection. '
            'Moved notes remain available offline on this device.',
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('Cancel'),
            ),
            OutlinedButton(
              key: const Key('start-empty-collection'),
              onPressed: () => Navigator.pop(context, false),
              child: const Text('Start with an empty collection'),
            ),
            FilledButton(
              key: const Key('move-local-notes'),
              onPressed: () => Navigator.pop(context, true),
              child: const Text('Move my current notes'),
            ),
          ],
        ),
      );
      if (move == null || !mounted) return;
      await _applySwitch(url, access: access, move: move);
      return;
    }
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
    await _applySwitch(url, access: access);
  }

  Future<void> _applySwitch(String? url, {String? access, bool move = false}) async {
    setState(() => _switching = true);
    try {
      if (move) {
        await widget.onMoveLocalToRelay!(url!, access: access);
      } else {
        await widget.onSwitchRelay(url, access: access);
      }
      if (mounted) Navigator.of(context).popUntil((route) => route.isFirst);
    } on Object catch (error) {
      if (!mounted) return;
      setState(() => _switching = false);
      final message = error is StateError
          ? error.message.toString()
          : error.toString();
      final messenger = ScaffoldMessenger.of(context);
      // A move may already be durable when starting the connection fails.
      // Return to the live collection instead of keeping stale local settings.
      if (move) Navigator.of(context).popUntil((route) => route.isFirst);
      messenger.showSnackBar(SnackBar(content: Text(
        move ? message : 'Could not switch relay: $message',
      )));
    }
  }

  Future<void> _saveAccess(String? access) async {
    final save = widget.onSetAccess;
    if (save == null || _switching) return;
    setState(() => _switching = true);
    try {
      await save(access);
      if (!mounted) return;
      // Back to the notes, which is where reconnecting shows; this screen
      // was drawn with the key the relay had before.
      final messenger = ScaffoldMessenger.of(context);
      Navigator.of(context).pop();
      messenger.showSnackBar(
        SnackBar(
          content: Text(
            access == null
                ? 'Access key removed.'
                : 'Access key saved. Reconnecting to the relay.',
          ),
        ),
      );
    } on Object catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not save the access key: $error')),
      );
    } finally {
      if (mounted) setState(() => _switching = false);
    }
  }

  /// Ask for the relay's key: typed, or pasted as the relay link it came in.
  Future<void> _enterAccess() async {
    final key = await showDialog<String>(
      context: context,
      builder: (context) => _AccessKeyDialog(
        read: (input) => _readAccess(input, relay: widget.profile.url),
      ),
    );
    if (key != null) await _saveAccess(key);
  }

  /// A key on its own, or a relay link for [relay] carrying one.
  ({String? access, String? error}) _readAccess(
    String input, {
    required String? relay,
  }) {
    final plain = normalizeAccessKey(input);
    if (plain != null) return (access: plain, error: null);
    try {
      final link = parseRelayLink(input);
      if (link.access == null) {
        return (access: null, error: 'That link has no access key in it.');
      }
      if (relay != null && link.url != relay) {
        return (
          access: null,
          error: 'That link is for another relay: ${link.url}.',
        );
      }
      return (access: link.access, error: null);
    } on FormatException {
      return (access: null, error: 'That is not an access key or relay link.');
    }
  }

  Future<void> _copyRelayLink(String link) async {
    await Clipboard.setData(ClipboardData(text: link));
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(
        content: Text(
          'Relay link copied. It lets whoever has it use this relay, so '
          'send it only to people you share lists with.',
        ),
      ),
    );
  }

  String? _validateAccess(String? value) {
    if ((value ?? '').trim().isEmpty) return null;
    return normalizeAccessKey(value!) == null
        ? 'That is not an access key.'
        : null;
  }

  String? _validate(String? value) {
    final String url;
    try {
      url = parseRelayLink(value ?? '').url;
    } on FormatException catch (error) {
      return error.message;
    }
    if (widget.servedOverHttps && url.startsWith('http://')) {
      return 'This page is served over HTTPS, so the browser will only reach '
          'a relay whose address starts with https://.';
    }
    if (widget.mobileRelease && url.startsWith('http://')) {
      return 'Notes only sync over an encrypted connection. Use a relay '
          'address that starts with https:// or wss://.';
    }
    return null;
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Server (relay)')),
    body: ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text('Current profile', style: Theme.of(context).textTheme.titleMedium),
        ListTile(
          contentPadding: EdgeInsets.zero,
          leading: const Icon(Icons.check_circle_outline),
          title: Text(widget.profile.label),
          subtitle: Text(widget.profile.url == null
              ? 'This collection stays on this device until you set up sync.'
              : 'Notes in other collections stay separate.'),
        ),
        if (widget.profile.url != null) ..._accessSection(context),
        const Divider(height: 32),
        Text(
          'Switch to a relay',
          style: Theme.of(context).textTheme.titleMedium,
        ),
        const SizedBox(height: 8),
        const Text(
          'A relay stores encrypted backups and can sync notes. You can use '
          'one without an account; create an account for device access and '
          'sharing.',
          key: Key('relay-account-explanation'),
        ),
        const SizedBox(height: 8),
        const Text(
          'EasyNotes does not provide a relay. Get an address from your '
          'organization or a relay provider. For sharing, use the same relay '
          'as the people you share notes with. You can continue on this device '
          'and connect a relay later.',
          key: Key('relay-address-help'),
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
                  labelText: 'Relay URL or relay link',
                  hintText: 'https://relay.example',
                  helperText:
                      'Use the same relay as the people you share notes with.',
                ),
                validator: _validate,
                onFieldSubmitted: (_) => _submitUrl(),
              ),
              TextFormField(
                key: const Key('relay-access-field'),
                controller: _access,
                enabled: !_switching,
                autocorrect: false,
                decoration: const InputDecoration(
                  labelText: 'Access key (if the relay has one)',
                  helperText:
                      'Get the key from the relay operator. A relay link '
                      'includes it. Leave empty if the relay is open.',
                  helperMaxLines: 2,
                ),
                validator: _validateAccess,
                onFieldSubmitted: (_) => _submitUrl(),
              ),
              const SizedBox(height: 12),
              Align(
                alignment: Alignment.centerRight,
                child: FilledButton(
                  key: const Key('switch-relay-button'),
                  onPressed: _switching ? null : _submitUrl,
                  child: Text(
                    widget.profile.url == null ? 'Connect relay' : 'Switch relay',
                  ),
                ),
              ),
            ],
          ),
        ),
        const Divider(height: 32),
        Text('Saved profiles', style: Theme.of(context).textTheme.titleMedium),
        for (final profile in widget.profiles)
          ListTile(
            contentPadding: EdgeInsets.zero,
            leading: Icon(
              profile.url == widget.profile.url
                  ? Icons.radio_button_checked
                  : Icons.radio_button_unchecked,
            ),
            title: Text(profile.label),
            subtitle: profile.url == widget.profile.url
                ? const Text('Selected')
                : widget.refuses(profile.url)
                ? const Text('Needs an https:// relay in this version')
                : null,
            enabled:
                !_switching &&
                profile.id != widget.profile.id &&
                !widget.refuses(profile.url),
            onTap: () => _switchTo(profile.url),
          ),
        if (_switching) const Center(child: CircularProgressIndicator()),
      ],
    ),
  );

  List<Widget> _accessSection(BuildContext context) {
    final link = widget.profile.relayLink;
    return [
      ListTile(
        key: const Key('relay-access-status'),
        contentPadding: EdgeInsets.zero,
        leading: Icon(link == null ? Icons.lock_open : Icons.key),
        title: Text(link == null ? 'No access key' : 'Access key saved'),
        subtitle: Text(
          link == null
              ? 'Enough for a relay anyone may use. If this relay asks for a '
                    'key, enter the one its operator gave you.'
              : 'Someone you share a list with needs this relay\'s link as '
                    'well as the invite.',
        ),
      ),
      Wrap(
        spacing: 8,
        children: [
          if (widget.onSetAccess != null)
            OutlinedButton(
              key: const Key('relay-access-enter'),
              onPressed: _switching ? null : _enterAccess,
              child: Text(
                link == null ? 'Enter access key' : 'Change access key',
              ),
            ),
          if (link != null)
            OutlinedButton(
              key: const Key('relay-link-copy'),
              onPressed: () => _copyRelayLink(link),
              child: const Text('Copy relay link'),
            ),
        ],
      ),
    ];
  }

  Future<void> _submitUrl() async {
    if (!(_form.currentState?.validate() ?? false)) return;
    try {
      final link = parseRelayLink(_url.text);
      final typed = _access.text.trim();
      await _switchTo(
        link.url,
        access:
            link.access ?? (typed.isEmpty ? null : normalizeAccessKey(typed)),
      );
    } on FormatException catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error.message)));
    }
  }
}

/// Asks for a relay's access key, and pops with it once it reads as one.
class _AccessKeyDialog extends StatefulWidget {
  const _AccessKeyDialog({required this.read});

  final ({String? access, String? error}) Function(String input) read;

  @override
  State<_AccessKeyDialog> createState() => _AccessKeyDialogState();
}

class _AccessKeyDialogState extends State<_AccessKeyDialog> {
  final _controller = TextEditingController();
  String? _error;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _save() {
    final read = widget.read(_controller.text);
    if (read.error != null) {
      setState(() => _error = read.error);
    } else {
      Navigator.pop(context, read.access);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Relay access key'),
    content: TextField(
      key: const Key('relay-access-dialog-field'),
      controller: _controller,
      autocorrect: false,
      decoration: InputDecoration(
        labelText: 'Access key or relay link',
        helperText: 'Whoever runs this relay can give you either.',
        helperMaxLines: 2,
        errorText: _error,
        errorMaxLines: 3,
      ),
      onSubmitted: (_) => _save(),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        key: const Key('relay-access-save'),
        onPressed: _save,
        child: const Text('Save'),
      ),
    ],
  );
}

