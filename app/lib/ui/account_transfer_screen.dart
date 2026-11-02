import 'dart:async';

import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:qr_flutter/qr_flutter.dart';

import '../core/invites.dart';
import '../core/app_visibility.dart';
import '../core/relay_profiles.dart';

/// One-time pairing from an existing device or QR scan on a new one.
class AccountTransferScreen extends StatefulWidget {
  const AccountTransferScreen({
    super.key,
    required this.library,
    required this.profile,
    required this.onMoveLocalToRelay,
    required this.onSetAccess,
  });

  final AccountTransfers library;
  final RelayProfile profile;
  final Future<void> Function(String address, {String? access})
  onMoveLocalToRelay;
  final Future<void> Function(String? access) onSetAccess;

  @override
  State<AccountTransferScreen> createState() => _AccountTransferScreenState();
}

class _AccountTransferScreenState extends State<AccountTransferScreen> {
  Timer? _poller;
  String? _qr;
  String? _code;
  String? _message;
  bool _busy = false;
  bool _approved = false;

  /// Whether this device belongs to an account.
  bool get _hasAccount {
    try {
      return widget.library is Accounts &&
          (widget.library as Accounts).hasAccount;
    } on Object {
      return false;
    }
  }

  /// The side of the pairing this screen was opened on. Only a device that
  /// arrived with an account and a relay can act as the owner. A guest stays
  /// on the guest's side after adopting, because it is still finishing its own
  /// transfer, and [widget.profile] is a snapshot a move here does not update.
  late final bool _owner = widget.profile.url != null && _hasAccount;

  @override
  void dispose() {
    _poller?.cancel();
    super.dispose();
  }

  Future<void> _startOwner() async {
    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      final qr = await widget.library.beginAccountTransfer(
        widget.profile.url!,
        widget.profile.access,
      );
      if (!mounted) return;
      setState(() {
        _qr = qr;
        _busy = false;
      });
      _poller = Timer.periodic(const Duration(seconds: 2), (_) => _pollOwner());
    } catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _message = '$e';
        });
      }
    }
  }

  Future<void> _pollOwner() async {
    if (_busy || _code != null || _approved || !appIsForeground.value) return;
    try {
      final code = await widget.library.accountTransferRequestCode();
      if (code != null && mounted) {
        _poller?.cancel();
        setState(() => _code = code);
      }
    } catch (e) {
      if (mounted) {
        _poller?.cancel();
        setState(() => _message = '$e');
      }
    }
  }

  Future<void> _approve() async {
    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      await widget.library.approveAccountTransfer();
      if (!mounted) return;
      setState(() {
        _busy = false;
        _approved = true;
        _message =
            'Transfer sent securely. The new device can now finish setup.';
      });
    } catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _message = '$e';
        });
      }
    }
  }

  Future<void> _startGuest() async {
    final qr = await Navigator.of(context).push<String>(
      MaterialPageRoute(builder: (_) => const _TransferQrScanner()),
    );
    if (qr == null || !mounted) return;
    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      final code = await widget.library.beginAccountTransferFromQr(qr);
      if (!mounted) return;
      setState(() {
        _busy = false;
        _code = code;
      });
      _poller = Timer.periodic(const Duration(seconds: 2), (_) => _pollGuest());
    } catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _message = '$e';
        });
      }
    }
  }

  Future<void> _pollGuest() async {
    if (_busy || _approved || !appIsForeground.value) return;
    setState(() => _busy = true);
    try {
      final transfer = await widget.library.pollAccountTransferResult();
      if (transfer == null || !mounted) {
        if (mounted) setState(() => _busy = false);
        return;
      }
      final current = widget.profile.url;
      final address = transfer.relayUrl;
      if (current != null && current != address) {
        throw StateError(
          'Choose the same sync server as the original device in Sync server '
          'settings, then scan again.',
        );
      }
      // The account goes on before the collection moves. A device with no
      // account that moves first is asked to create one, and adopting the
      // transferred account is then refused: the transfer cannot finish.
      // An account already on this device is the same account paired again
      // after a failure, and adopting it a second time is refused just the
      // same, so only a device without one adopts.
      if (!_hasAccount) await widget.library.adoptAccountTransfer();
      _poller?.cancel();
      // Paired: no longer waiting for approval, whether or not the move follows.
      if (mounted) setState(() => _code = null);
      try {
        if (current == null) {
          await widget.onMoveLocalToRelay(
            address,
            access: transfer.relayAccess,
          );
        } else {
          await widget.onSetAccess(transfer.relayAccess);
        }
      } on Object catch (e) {
        // The account is on this device; what is left is the move or the
        // connection. True whether it stopped before the profile moved or
        // after it, when the notes are already on the relay.
        throw StateError(
          'This device has your account, but the transfer to $address did not '
          'finish: $e Open Sync server settings to finish.',
        );
      }
      if (!mounted) return;
      if (current == null) {
        // The library moved, so the screen that opened this one holds a
        // collection that has since closed. Report the move on the way out.
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text(
              'Account added. This device is connected to ${transfer.relayUrl}.',
            ),
          ),
        );
        Navigator.of(context).pop(true);
        return;
      }
      setState(() {
        _busy = false;
        _approved = true;
        _message =
            'Account added. This device is connected to ${transfer.relayUrl}.';
      });
    } catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _message = '$e';
        });
      }
    }
  }

  Future<void> _cancel() async {
    _poller?.cancel();
    try {
      await widget.library.cancelAccountTransfer();
    } catch (_) {}
    if (mounted) Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final owner = _owner;
    return Scaffold(
      appBar: AppBar(
        title: Text(owner ? 'Add another device' : 'Add this device'),
      ),
      body: ListView(
        padding: const EdgeInsets.all(24),
        children: [
          Text(
            owner
                ? 'Scan this QR code with your new device. The account key and server access key are encrypted for that device. Keep this screen open until you approve the request.'
                : 'Scan the QR code shown by your original device. Both devices will show a matching code; compare them before approving on the original device.',
          ),
          const SizedBox(height: 20),
          if (owner && _qr == null)
            FilledButton.icon(
              key: const Key('begin-account-transfer'),
              onPressed: _busy || widget.profile.url == null
                  ? null
                  : _startOwner,
              icon: const Icon(Icons.qr_code_2),
              label: const Text('Show one-time QR code'),
            ),
          if (owner && _qr != null) ...[
            Center(
              child: Container(
                color: Colors.white,
                padding: const EdgeInsets.all(12),
                child: QrImageView(
                  data: _qr!,
                  size: 250,
                  errorCorrectionLevel: QrErrorCorrectLevel.M,
                ),
              ),
            ),
            const SizedBox(height: 20),
            if (_code == null)
              const Center(child: CircularProgressIndicator())
            else ...[
              const Text('Check that the new device shows this same code:'),
              const SizedBox(height: 8),
              Center(
                child: Text(
                  _code!,
                  key: const Key('account-transfer-code'),
                  style: Theme.of(
                    context,
                  ).textTheme.headlineLarge?.copyWith(letterSpacing: 5),
                ),
              ),
              const SizedBox(height: 12),
              FilledButton(
                key: const Key('approve-account-transfer'),
                onPressed: _busy ? null : _approve,
                child: const Text('Approve transfer'),
              ),
            ],
          ],
          if (!owner && !_approved)
            FilledButton.icon(
              key: const Key('scan-account-transfer'),
              onPressed: _busy ? null : _startGuest,
              icon: const Icon(Icons.qr_code_scanner),
              label: const Text('Scan original device'),
            ),
          if (_code != null && !owner && !_approved) ...[
            const SizedBox(height: 24),
            const Text(
              'Compare this code with the original device. Approve there only if they match.',
            ),
            const SizedBox(height: 8),
            Center(
              child: Text(
                _code!,
                key: const Key('account-transfer-code'),
                style: Theme.of(
                  context,
                ).textTheme.headlineLarge?.copyWith(letterSpacing: 5),
              ),
            ),
            const SizedBox(height: 20),
            const Center(child: CircularProgressIndicator()),
            const Center(child: Text('Waiting for approval…')),
          ],
          if (_busy && _code == null) const LinearProgressIndicator(),
          if (_message != null) ...[
            const SizedBox(height: 16),
            Text(_message!, key: const Key('account-transfer-message')),
          ],
          if (!_approved) ...[
            const SizedBox(height: 20),
            TextButton(onPressed: _cancel, child: const Text('Cancel')),
          ],
        ],
      ),
    );
  }
}

class _TransferQrScanner extends StatefulWidget {
  const _TransferQrScanner();
  @override
  State<_TransferQrScanner> createState() => _TransferQrScannerState();
}

class _TransferQrScannerState extends State<_TransferQrScanner> {
  bool _handled = false;
  void _scan(BarcodeCapture capture) {
    if (_handled) return;
    final value = capture.barcodes
        .map((b) => b.rawValue)
        .whereType<String>()
        .firstOrNull;
    if (value == null || value.isEmpty) return;
    _handled = true;
    Navigator.of(context).pop(value);
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Scan transfer QR code')),
    body: MobileScanner(onDetect: _scan),
  );
}
