/// First-run relay setup: an address, then its access key, then the handshake
/// that proves the relay is there — before anything is committed.
library;

import 'package:flutter/foundation.dart'
    show TargetPlatform, defaultTargetPlatform, kIsWeb, kReleaseMode;
import 'package:flutter/material.dart';

import '../core/relay_profiles.dart';

/// Whether the page this app was served from is HTTPS, in which case a browser
/// will refuse it a plain `http://` or `ws://` relay (mixed content).
///
/// The same two rules as Settings → Relay, kept here as well so the first-run
/// path refuses an address for the same reason that screen does.
bool get _servedOverHttps => kIsWeb && Uri.base.scheme == 'https';

/// Whether this is a release build for a phone, whose operating system refuses
/// plain `http://` and `ws://` outright.
bool get _mobileRelease =>
    kReleaseMode &&
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS);

enum _Step { url, access, verify }

/// The path from "Set up cloud backups" to a connected relay.
///
/// Three screens, in the order a person can answer them: the address, its
/// access key if the relay has one, then the handshake. Nothing is committed
/// until the handshake passes, so an abort leaves the collection on "On this
/// device" with no broken profile behind it.
///
/// Pops with `true` once the relay is verified and selected, and with `false`
/// or null when the person gives up.
class RelaySetupWizard extends StatefulWidget {
  RelaySetupWizard({
    super.key,
    required this.onSwitchRelay,
    required this.onVerifyRelay,
    bool? servedOverHttps,
    bool? mobileRelease,
  }) : servedOverHttps = servedOverHttps ?? _servedOverHttps,
       mobileRelease = mobileRelease ?? _mobileRelease;

  /// Select the relay at a canonical URL, keeping [access] as its key.
  final Future<void> Function(String?, {String? access}) onSwitchRelay;

  /// Prove the address and key to the relay without committing anything.
  final Future<void> Function(String url, {String? access}) onVerifyRelay;

  /// A browser page served over HTTPS can only reach a relay with TLS; a
  /// released phone app likewise. The same rules as Settings → Relay.
  final bool servedOverHttps;

  /// A released phone app cannot reach a relay without TLS either.
  final bool mobileRelease;

  @override
  State<RelaySetupWizard> createState() => _RelaySetupWizardState();
}

class _RelaySetupWizardState extends State<RelaySetupWizard> {
  final _form = GlobalKey<FormState>();
  final _url = TextEditingController();
  final _access = TextEditingController();

  _Step _step = _Step.url;

  /// The address as read at step one, and the key as read at step two. Both
  /// survive a failed verification so a retry does not start over.
  String? _canonical;
  String? _accessKey;

  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _url.dispose();
    _access.dispose();
    super.dispose();
  }

  String get _title => switch (_step) {
    _Step.url => 'Relay address',
    _Step.access => 'Relay access key',
    _Step.verify => 'Connecting',
  };

  String? _validateUrl(String? value) {
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

  String? _validateAccess(String? value) => _readAccess(value ?? '').error;

  /// A key on its own, or a relay link carrying one for the address already
  /// given — the same reading Settings → Relay does.
  ({String? key, String? error}) _readAccess(String input) {
    final raw = input.trim();
    if (raw.isEmpty) return (key: null, error: null);
    final plain = normalizeAccessKey(raw);
    if (plain != null) return (key: plain, error: null);
    try {
      final link = parseRelayLink(raw);
      if (link.access == null) {
        return (key: null, error: 'That link has no access key in it.');
      }
      if (link.url != _canonical) {
        return (
          key: null,
          error: 'That link is for another relay: ${link.url}.',
        );
      }
      return (key: link.access, error: null);
    } on FormatException {
      return (key: null, error: 'That is not an access key or relay link.');
    }
  }

  /// Read the address, and carry an access key that came inside a relay link
  /// into the next step rather than asking for it again.
  void _submitUrl() {
    if (_busy) return;
    if (!(_form.currentState?.validate() ?? false)) return;
    final link = parseRelayLink(_url.text);
    _canonical = link.url;
    if (link.access != null && _access.text.trim().isEmpty) {
      _access.text = link.access!;
    }
    setState(() => _step = _Step.access);
  }

  void _submitAccess() {
    if (_busy) return;
    if (!(_form.currentState?.validate() ?? false)) return;
    final read = _readAccess(_access.text);
    _accessKey = read.key;
    setState(() {
      _step = _Step.verify;
      _error = null;
    });
    _verify();
  }

  /// Prove the relay, then commit to it. A failure at either point stays on
  /// the verification screen with the words that came back.
  Future<void> _verify() async {
    if (_busy) return;
    final url = _canonical;
    if (url == null) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.onVerifyRelay(url, access: _accessKey);
      await widget.onSwitchRelay(url, access: _accessKey);
    } on Object catch (error) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        _error = error is StateError ? error.message.toString() : '$error';
      });
      return;
    }
    if (!mounted) return;
    Navigator.of(context).pop(true);
  }

  /// Back one step, or out of the setup from the first.
  void _back() {
    if (_busy) return;
    switch (_step) {
      case _Step.verify:
        setState(() {
          _step = _Step.access;
          _error = null;
        });
      case _Step.access:
        setState(() => _step = _Step.url);
      case _Step.url:
        Navigator.of(context).pop(false);
    }
  }

  void _abort() => Navigator.of(context).pop(false);

  @override
  Widget build(BuildContext context) => PopScope(
    canPop: _step == _Step.url && !_busy,
    onPopInvokedWithResult: (didPop, _) {
      if (!didPop) _back();
    },
    child: Scaffold(
      appBar: AppBar(
        title: Text(_title),
        leading: _step == _Step.url
            ? const BackButton()
            : IconButton(
                icon: const Icon(Icons.arrow_back),
                tooltip: 'Back',
                onPressed: _busy ? null : _back,
              ),
      ),
      body: switch (_step) {
        _Step.url => _urlBody(),
        _Step.access => _accessBody(),
        _Step.verify => _verifyBody(),
      },
    ),
  );

  Widget _urlBody() => Form(
    key: _form,
    child: ListView(
      padding: const EdgeInsets.all(24),
      children: [
        const Text(
          'EasyNotes does not provide a relay. Get an address from your '
          'organization or a relay provider.',
          key: Key('relay-setup-intro'),
        ),
        const SizedBox(height: 16),
        TextFormField(
          key: const Key('relay-setup-url'),
          controller: _url,
          autofocus: true,
          keyboardType: TextInputType.url,
          autocorrect: false,
          decoration: const InputDecoration(
            labelText: 'Relay URL',
            hintText: 'https://relay.example',
            helperText:
                'Use the same relay as the people you share notes with.',
            helperMaxLines: 2,
          ),
          validator: _validateUrl,
          onFieldSubmitted: (_) => _submitUrl(),
        ),
        const SizedBox(height: 24),
        FilledButton(
          key: const Key('relay-setup-url-continue'),
          onPressed: _submitUrl,
          child: const Text('Continue'),
        ),
      ],
    ),
  );

  Widget _accessBody() => Form(
    key: _form,
    child: ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text(
          'Relay: ${_canonical ?? ''}',
          key: const Key('relay-setup-address'),
        ),
        const SizedBox(height: 16),
        const Text(
          'If the relay operator gave you an access key, enter it here. Leave '
          'it empty for a relay anyone may use.',
          key: Key('relay-setup-access-help'),
        ),
        const SizedBox(height: 16),
        TextFormField(
          key: const Key('relay-setup-access'),
          controller: _access,
          autofocus: true,
          autocorrect: false,
          decoration: const InputDecoration(
            labelText: 'Access key (optional)',
            helperText: 'A relay link includes the key and can be pasted here.',
            helperMaxLines: 2,
          ),
          validator: _validateAccess,
          onFieldSubmitted: (_) => _submitAccess(),
        ),
        const SizedBox(height: 24),
        FilledButton(
          key: const Key('relay-setup-access-continue'),
          onPressed: _submitAccess,
          child: const Text('Connect'),
        ),
      ],
    ),
  );

  Widget _verifyBody() {
    if (_busy) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const CircularProgressIndicator(),
              const SizedBox(height: 24),
              Text(
                'Checking ${_canonical ?? ''}…',
                key: const Key('relay-setup-checking'),
                textAlign: TextAlign.center,
              ),
              const SizedBox(height: 8),
              const Text(
                'This confirms the relay is reachable and accepts your access '
                'key.',
                textAlign: TextAlign.center,
              ),
            ],
          ),
        ),
      );
    }
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Icon(
          Icons.cloud_off_outlined,
          size: 48,
          color: Theme.of(context).colorScheme.error,
        ),
        const SizedBox(height: 16),
        Text(
          'Could not connect to this relay',
          key: const Key('relay-setup-failed'),
          textAlign: TextAlign.center,
          style: Theme.of(context).textTheme.titleMedium,
        ),
        const SizedBox(height: 12),
        Text(
          '${_canonical ?? ''}\n$_error',
          key: const Key('relay-setup-error'),
          textAlign: TextAlign.center,
        ),
        const SizedBox(height: 24),
        FilledButton(
          key: const Key('relay-setup-retry'),
          onPressed: () => setState(() {
            _step = _Step.url;
            _error = null;
          }),
          child: const Text('Go back and try again'),
        ),
        const SizedBox(height: 8),
        OutlinedButton(
          key: const Key('relay-setup-abort'),
          onPressed: _abort,
          child: const Text('Abort setup'),
        ),
      ],
    );
  }
}
