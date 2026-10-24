/// Setting up a relay: an address, then its access key, then the handshake
/// that proves the relay is there — before anything is committed.
///
/// The same flow is used everywhere a relay is chosen: the first run connects
/// to one, Settings connects to another or changes the key of the one in use.
/// Nothing is committed until the handshake passes, so an abort leaves the
/// collection where it was.
library;

import 'package:flutter/material.dart';

import '../core/relay_profiles.dart';
import '../core/relay_session.dart' show SyncSetupCancelled;
import 'relay_address.dart';

/// What the wizard was opened to do.
enum RelaySetupMode {
  /// Use the first relay for a collection that has none. Offers to take the
  /// notes on this device with it.
  connect,

  /// Move to another relay. Each relay has notes and an account of its own.
  switchServer,

  /// Check a new access key for the relay already in use, then keep it.
  accessKey,
}

enum _Step { url, access, verify, move }

/// The path from "Set up sync" to a connected relay.
///
/// The address comes first because the key may arrive inside it, and the
/// handshake comes last because it is the only thing that can prove the first
/// two were right.
///
/// Pops with `true` once the relay is verified and selected, and with `false`
/// or null when the person gives up.
class RelaySetupWizard extends StatefulWidget {
  RelaySetupWizard({
    super.key,
    required this.onSwitchRelay,
    required this.onVerifyRelay,
    this.onSetAccess,
    this.onMoveLocalToRelay,
    this.mode = RelaySetupMode.connect,
    this.profile,
    this.profiles = const [],
    this.hasLocalNotes = false,
    bool? servedOverHttps,
    bool? mobileRelease,
  }) : assert(
         mode != RelaySetupMode.accessKey ||
             (onSetAccess != null && profile?.url != null),
         'changing a key needs a relay and somewhere to keep the key',
       ),
       servedOverHttps = servedOverHttps ?? platformServedOverHttps,
       mobileRelease = mobileRelease ?? platformMobileRelease;

  /// Select the relay at a canonical URL, keeping [access] as its key.
  final Future<void> Function(String?, {String? access}) onSwitchRelay;

  /// Prove the address and key to the relay without committing anything.
  final Future<void> Function(String url, {String? access}) onVerifyRelay;

  /// Keep [access] for the relay already in use, or forget it when null. Only
  /// needed in [RelaySetupMode.accessKey].
  final Future<void> Function(String?)? onSetAccess;

  /// Assign the current local collection to a new relay, keeping its notes.
  /// Never offered for a destination that already exists.
  final Future<void> Function(String, {String? access})? onMoveLocalToRelay;

  final RelaySetupMode mode;

  /// The profile in use: its address is what an access key belongs to, and
  /// it is what "already using this server" compares against.
  final RelayProfile? profile;

  /// The saved profiles, so that "this server starts empty" is only said
  /// about a server that does.
  final List<RelayProfile> profiles;

  /// Whether the current collection has notes that "move my notes" could
  /// take along. A first run with an empty grid is not asked a question about
  /// notes that do not exist.
  final bool hasLocalNotes;

  /// A browser page served over HTTPS can only reach a relay with TLS; a
  /// released phone app likewise.
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

  late _Step _step = switch (widget.mode) {
    RelaySetupMode.accessKey => _Step.access,
    _ => _Step.url,
  };

  /// The address as read at step one, and the key as read at step two. Both
  /// survive a failed verification so a retry does not start over.
  String? _canonical;
  String? _accessKey;

  bool _busy = false;
  String? _error;
  String _failureTitle = 'Could not connect to this sync server';

  @override
  void initState() {
    super.initState();
    if (widget.mode == RelaySetupMode.accessKey) {
      _canonical = widget.profile?.url;
      _accessKey = widget.profile?.access;
    }
  }

  @override
  void dispose() {
    _url.dispose();
    _access.dispose();
    super.dispose();
  }

  bool get _changingKey => widget.mode == RelaySetupMode.accessKey;

  String get _title => switch (_step) {
    _Step.url =>
      widget.mode == RelaySetupMode.switchServer
          ? 'Switch sync server'
          : 'Sync server address',
    _Step.access => 'Sync server access key',
    _Step.verify => _changingKey ? 'Checking access key' : 'Connecting',
    _Step.move => 'Your notes',
  };

  /// The host of the address being connected to: what a person recognises.
  String? get _host {
    final url = _canonical;
    if (url == null) return null;
    final host = Uri.tryParse(url)?.host;
    return host == null || host.isEmpty ? url : host;
  }

  /// Whether the current local collection can go to the new relay rather than
  /// leaving it behind.
  bool get _canOfferMove {
    final move = widget.onMoveLocalToRelay;
    final url = _canonical;
    return widget.mode == RelaySetupMode.connect &&
        widget.profile?.url == null &&
        widget.hasLocalNotes &&
        move != null &&
        url != null &&
        !widget.profiles.any((profile) => profile.url == url);
  }

  String? _validateUrl(String? value) {
    final error = relayAddressError(
      value,
      servedOverHttps: widget.servedOverHttps,
      mobileRelease: widget.mobileRelease,
    );
    if (error != null) return error;
    // A typed address that is the one already in use has nowhere to switch
    // to: saying so here is kinder than a spinner and a screen that does not
    // change.
    if (widget.mode == RelaySetupMode.switchServer &&
        _canonicalOr(value) == widget.profile?.url) {
      return 'You are already using this sync server.';
    }
    return null;
  }

  /// [value] as a canonical relay address, without throwing: validation has
  /// already accepted it.
  static String? _canonicalOr(String? value) {
    try {
      return parseRelayLink(value ?? '').url;
    } on FormatException {
      return null;
    }
  }

  String? _validateAccess(String? value) =>
      readAccessKey(value ?? '', relay: _canonical).error;

  /// A key on its own, or a relay link carrying one for the address already
  /// given.
  ({String? key, String? error}) _readAccess(String input) =>
      readAccessKey(input, relay: _canonical);

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
    _failureTitle = _changingKey
        ? 'Could not check this access key'
        : 'Could not connect to this sync server';
    setState(() {
      _step = _Step.verify;
      _error = null;
    });
    _verify();
  }

  /// Prove the relay, then commit. A failure at either point stays on the
  /// verification screen with the words that came back.
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
    } on Object catch (error) {
      _fail(error);
      return;
    }
    if (_changingKey) {
      try {
        await widget.onSetAccess!(_accessKey);
      } on Object catch (error) {
        _failureTitle = 'Could not save the access key';
        _fail(error);
        return;
      }
      if (!mounted) return;
      Navigator.of(context).pop(true);
      return;
    }
    if (_canOfferMove) {
      // Nothing is committed yet: the notes that would come along are a
      // choice, and it comes after the server has answered.
      if (!mounted) return;
      setState(() {
        _busy = false;
        _step = _Step.move;
      });
      return;
    }
    await _commit(move: false);
  }

  Future<void> _commit({required bool move}) async {
    final url = _canonical;
    if (url == null) return;
    setState(() {
      _busy = true;
      _error = null;
      _failureTitle = move
          ? 'Could not move your notes'
          : 'Could not switch to this sync server';
    });
    try {
      if (move) {
        await widget.onMoveLocalToRelay!(url, access: _accessKey);
      } else {
        await widget.onSwitchRelay(url, access: _accessKey);
      }
    } on SyncSetupCancelled {
      // The person backed out of the account step before an account existed.
      // Nothing was committed, so this is a choice rather than a failure:
      // return to the address they can change or leave.
      if (!mounted) return;
      setState(() {
        _busy = false;
        _step = _Step.url;
        _error = null;
      });
      return;
    } on Object catch (error) {
      // A move may already be durable when starting sync fails. Retrying the
      // same move cannot work — this collection is no longer local — so this
      // is reported and closed, not offered again.
      _fail(error, moved: move);
      return;
    }
    if (!mounted) return;
    Navigator.of(context).pop(true);
  }

  /// A move that failed after it was written: the collection is this relay's
  /// now, so there is nothing to retry here.
  bool _moveFailed = false;

  void _fail(Object error, {bool moved = false}) {
    if (!mounted) return;
    setState(() {
      _busy = false;
      _step = _Step.verify;
      _moveFailed = moved;
      _error = error is StateError ? error.message.toString() : '$error';
    });
  }

  /// Where "go back and try again" goes: the step whose answer was wrong.
  _Step get _retryStep => _changingKey ? _Step.access : _Step.url;

  /// Back one step, or out of the setup from the first.
  void _back() {
    if (_busy) return;
    switch (_step) {
      case _Step.verify:
        setState(() {
          _step = _retryStep;
          _error = null;
        });
      case _Step.move:
        setState(() => _step = _Step.url);
      case _Step.access:
        if (_changingKey) {
          Navigator.of(context).pop(false);
        } else {
          setState(() => _step = _Step.url);
        }
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
        _Step.move => _moveBody(),
      },
    ),
  );

  Widget _urlBody() => Form(
    key: _form,
    child: ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text(
          widget.mode == RelaySetupMode.switchServer
              ? 'Each sync server has notes and an account of its own. '
                    'Switching opens that collection; your notes here stay '
                    'in this one.'
              : 'EasyNotes does not provide a sync server. Get an address '
                    'from your organization or a sync server provider.',
          key: const Key('relay-setup-intro'),
        ),
        const SizedBox(height: 16),
        TextFormField(
          key: const Key('relay-setup-url'),
          controller: _url,
          autofocus: true,
          keyboardType: TextInputType.url,
          autocorrect: false,
          decoration: const InputDecoration(
            labelText: 'Server address',
            hintText: 'https://sync.example.com',
            helperText:
                'Use the same server as the people you share notes with.',
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
          'Server: ${_canonical ?? ''}',
          key: const Key('relay-setup-address'),
        ),
        const SizedBox(height: 16),
        if (_changingKey)
          const Text(
            'The new key is checked with the server before it is saved. '
            'Leave the field empty to remove the key.',
            key: Key('relay-setup-access-help'),
          )
        else ...[
          const Text(
            'If whoever runs the server gave you an access key, enter it here. '
            'Leave it empty for a server anyone may use.',
            key: Key('relay-setup-access-help'),
          ),
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(
              _destinationExists
                  ? 'This server already has a collection saved here. Its '
                        'notes open once you connect.'
                  : 'This server starts with an empty collection.',
              key: const Key('relay-setup-destination'),
            ),
          ),
        ],
        const SizedBox(height: 16),
        TextFormField(
          key: const Key('relay-setup-access'),
          controller: _access,
          autofocus: true,
          autocorrect: false,
          decoration: const InputDecoration(
            labelText: 'Access key (optional)',
            helperText:
                'A server link includes the key and can be pasted here.',
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
        if (_changingKey && widget.profile?.access != null) ...[
          const SizedBox(height: 8),
          TextButton(
            key: const Key('relay-setup-access-remove'),
            onPressed: _busy ? null : _removeAccess,
            child: const Text('Remove access key'),
          ),
        ],
      ],
    ),
  );

  bool get _destinationExists =>
      widget.profiles.any((profile) => profile.url == _canonical);

  /// Forget the key of the relay in use, after saying what that means. No
  /// verification: there is no key to verify, and the next sync reports a
  /// server that insists on one.
  Future<void> _removeAccess() async {
    final remove = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Remove access key?'),
        content: const Text(
          'The server will be used without a key. If it asks for one, sync '
          'stops until you enter it again.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            key: const Key('relay-setup-access-remove-confirm'),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (remove != true || !mounted) return;
    setState(() => _busy = true);
    try {
      await widget.onSetAccess!(null);
    } on Object catch (error) {
      _failureTitle = 'Could not remove the access key';
      _fail(error);
      return;
    }
    if (!mounted) return;
    Navigator.of(context).pop(true);
  }

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
                'This confirms the server is reachable and accepts your access '
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
          _failureTitle,
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
        if (_moveFailed)
          FilledButton(
            key: const Key('relay-setup-close'),
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('Close'),
          )
        else ...[
          FilledButton(
            key: const Key('relay-setup-retry'),
            onPressed: () => setState(() {
              _step = _retryStep;
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
      ],
    );
  }

  /// The last question for a first relay: take the notes already here along,
  /// or keep them where they are. The server has answered by this point, so
  /// neither answer can be wasted on a typo.
  Widget _moveBody() => ListView(
    padding: const EdgeInsets.all(24),
    children: [
      Icon(
        Icons.folder_copy_outlined,
        size: 48,
        color: Theme.of(context).colorScheme.primary,
      ),
      const SizedBox(height: 16),
      Text(
        'What about the notes on this device?',
        key: const Key('relay-setup-move-question'),
        textAlign: TextAlign.center,
        style: Theme.of(context).textTheme.titleMedium,
      ),
      const SizedBox(height: 12),
      Text(
        'They can move to ${_host ?? 'this server'}, attachments, labels and '
        'reminders included. Moved notes stay available offline here.',
        textAlign: TextAlign.center,
      ),
      const SizedBox(height: 24),
      FilledButton(
        key: const Key('move-local-notes'),
        onPressed: _busy ? null : () => _commit(move: true),
        child: const Text('Move my current notes'),
      ),
      const SizedBox(height: 8),
      OutlinedButton(
        key: const Key('start-empty-collection'),
        onPressed: _busy ? null : () => _commit(move: false),
        child: const Text('Start with an empty collection'),
      ),
    ],
  );
}
