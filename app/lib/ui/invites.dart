/// Sharing a note with another person, from both ends.
///
/// The owner's side is [CollaboratorsDialog]: make a link, send it however they like,
/// and let in whoever asks — after checking the six digits, if they are
/// careful. The guest's side is [JoinDialog] and [WaitingDialog]: paste the
/// link, say who you are, and wait to be let in. [InviteBanners] is what sits
/// above the grid while either side is waiting, so that closing a dialog does
/// not mean missing the answer.
///
/// All of it polls. There is no push channel yet — push wake-ups are out of
/// scope for this slice — and a person waiting on someone else is looking at
/// the screen anyway. The polls stop when the widget goes.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/invite_links.dart';
import '../core/invites.dart';

/// How often a screen that is waiting asks the relay. Short, because somebody
/// is looking at it; a variable, so a widget test can make it shorter still.
Duration invitePollInterval = const Duration(seconds: 3);

/// How often the list screen asks, with nobody necessarily looking.
Duration inviteBannerPollInterval = const Duration(seconds: 15);

String _days(Duration left) {
  if (left.inDays >= 2) return '${left.inDays} days';
  if (left.inHours >= 2) return '${left.inHours} hours';
  if (left.inMinutes >= 2) return '${left.inMinutes} minutes';
  return 'a moment';
}

/// Everything the owner of a note does about inviting someone to it.
class CollaboratorsDialog extends StatefulWidget {
  const CollaboratorsDialog({
    super.key,
    required this.invites,
    required this.noteId,
  });

  final Invites invites;
  final String noteId;

  @override
  State<CollaboratorsDialog> createState() => _CollaboratorsDialogState();
}

class _CollaboratorsDialogState extends State<CollaboratorsDialog> {
  Collaborators? _directory;
  String? _directoryError;
  Timer? _directoryPoll;
  Invite? _invite;
  InviteRequests? _requests;
  String? _error;

  /// Who was let in, for the line that says so once the invite has closed.
  String? _admitted;
  bool _revoked = false;
  bool _busy = false;
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    _loadDirectory();
    _directoryPoll = Timer.periodic(invitePollInterval, (_) {
      if (mounted) setState(_loadDirectory);
    });
    // The newest open invite, if this note has one: a person who closed the
    // dialog and came back should find the link they sent, not a new one.
    final open = widget.invites.invitesFor(widget.noteId);
    if (open.isNotEmpty) _watch(open.last);
  }

  @override
  void dispose() {
    _directoryPoll?.cancel();
    _poll?.cancel();
    super.dispose();
  }

  void _loadDirectory() {
    try {
      _directory = widget.invites.collaborators(widget.noteId);
      _directoryError = null;
    } catch (_) {
      _directory = null;
      _directoryError = 'Could not load collaborators. Retrying…';
    }
  }

  List<Widget> _people() {
    final directory = _directory;
    if (directory == null) {
      return [Text(_directoryError ?? 'Loading collaborators…')];
    }
    return [
      Text('Who has access', style: Theme.of(context).textTheme.titleSmall),
      ListTile(
        contentPadding: EdgeInsets.zero,
        leading: const CircleAvatar(child: Icon(Icons.person_outline)),
        title: Text(directory.canManage ? 'You' : 'Note owner'),
        subtitle: const Text('Owner · manages access'),
      ),
      for (final person in directory.people)
        ListTile(
          key: Key('collaborator-${person.id}'),
          contentPadding: EdgeInsets.zero,
          leading: CircleAvatar(child: Text(person.initials)),
          title: Text(
            '${person.displayName}${person.id == directory.currentDevice ? ' (you)' : ''}',
          ),
          subtitle: Text(
            'Can edit · Device ${person.id.length > 8 ? person.id.substring(0, 8) : person.id}',
          ),
        ),
      if (!directory.canManage &&
          !directory.people.any((p) => p.id == directory.currentDevice))
        const ListTile(
          contentPadding: EdgeInsets.zero,
          leading: CircleAvatar(child: Icon(Icons.person_outline)),
          title: Text('You'),
          subtitle: Text('Can edit'),
        ),
      if (directory.people.isEmpty)
        const Text(
          'No collaborators recorded yet.',
          key: Key('collaborators-empty'),
        ),
      const SizedBox(height: 8),
      Text(
        'Names are chosen by collaborators. Each entry is one device. Older shares may not appear in this list.',
        style: Theme.of(context).textTheme.bodySmall,
      ),
      if (_directoryError != null) Text(_directoryError!),
      if (!directory.canManage) ...[
        const SizedBox(height: 12),
        const Text('Only the owner can add collaborators or remove access.'),
      ],
      const Divider(height: 28),
    ];
  }

  void _watch(Invite invite) {
    _invite = invite;
    _poll?.cancel();
    _poll = Timer.periodic(invitePollInterval, (_) => _refresh());
    _refresh();
  }

  Future<void> _refresh() async {
    final invite = _invite;
    if (invite == null) return;
    try {
      final requests = await widget.invites.inviteRequests(invite.id);
      if (!mounted || _invite != invite) return;
      setState(() {
        _requests = requests;
        _error = null;
        if (!requests.open) {
          _poll?.cancel();
          if (_admitted == null) {
            final approved = requests.requests
                .where((r) => r.state == JoinRequestState.approved)
                .firstOrNull;
            if (approved == null) {
              _error = 'This invite has expired.';
            } else {
              _admitted = approved.name;
            }
          }
          _invite = null;
        }
      });
    } catch (e) {
      if (!mounted || _invite != invite) return;
      // Offline is the ordinary case and the next tick will try again; an
      // invite the relay has lost is not, and the core has forgotten it.
      setState(() => _error = '$e');
      if (widget.invites
          .invitesFor(widget.noteId)
          .every((i) => i.id != invite.id)) {
        _poll?.cancel();
        setState(() => _invite = null);
      }
    }
  }

  Future<void> _act(Future<void> Function() action) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await action();
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _loadDirectory();
        });
      }
    }
  }

  Future<void> _create() => _act(() async {
    final invite = await widget.invites.createInvite(widget.noteId);
    if (!mounted) return;
    setState(() {
      _admitted = null;
      _revoked = false;
      _requests = null;
      _error = null;
    });
    _watch(invite);
  });

  Future<void> _approve(JoinRequest request) => _act(() async {
    await widget.invites.approveRequest(_invite!.id, request.id);
    if (!mounted) return;
    _poll?.cancel();
    setState(() {
      _admitted = request.name;
      _invite = null;
      _requests = null;
    });
  });

  Future<void> _decline(JoinRequest request) => _act(() async {
    await widget.invites.declineRequest(_invite!.id, request.id);
    await _refresh();
  });

  Future<void> _withdraw() => _act(() async {
    await widget.invites.withdrawInvite(_invite!.id);
    if (!mounted) return;
    _poll?.cancel();
    setState(() {
      _invite = null;
      _requests = null;
    });
  });

  Future<void> _revoke() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Remove everyone else?'),
        content: const Text(
          'They will lose this note and cannot read new changes. This cannot '
          'erase anything they already copied or decrypted.',
        ),
        actions: [
          TextButton(
            key: const Key('cancel-revoke'),
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Keep sharing'),
          ),
          FilledButton(
            key: const Key('confirm-revoke'),
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('Remove everyone'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    await _act(() async {
      final invite = _invite;
      if (invite != null) await widget.invites.withdrawInvite(invite.id);
      await widget.invites.revokeCollaborators(widget.noteId);
      if (!mounted) return;
      _poll?.cancel();
      setState(() {
        _invite = null;
        _requests = null;
        _admitted = null;
        _revoked = true;
      });
    });
  }

  Widget _request(JoinRequest request) {
    final name = request.name.isEmpty ? 'Someone' : request.name;
    return Card(
      key: Key('join-request-${request.id}'),
      margin: const EdgeInsets.only(top: 8),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              '$name wants to join',
              style: Theme.of(context).textTheme.titleSmall,
            ),
            const SizedBox(height: 4),
            if (request.readable)
              Text(
                'Their screen shows ${request.code}. If you are not sure it is '
                'them, ask what theirs says.',
                key: Key('join-code-${request.id}'),
              )
            else
              const Text('This request was not made with your link.'),
            const SizedBox(height: 8),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  key: Key('decline-${request.id}'),
                  onPressed: _busy ? null : () => _decline(request),
                  child: const Text('Decline'),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  key: Key('approve-${request.id}'),
                  onPressed: _busy || !request.readable
                      ? null
                      : () => _approve(request),
                  child: const Text('Approve'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  List<Widget> _content() {
    final invite = _invite;
    if (invite == null) {
      return [
        if (_revoked)
          const Text(
            'Everyone else has been removed. New changes use a new key.',
            key: Key('sharing-revoked'),
          )
        else if (_admitted != null)
          Text(
            '${_admitted!.isEmpty ? 'They' : _admitted} can open this note now.',
            key: const Key('invite-admitted'),
          )
        else
          const Text(
            'Make a link and send it to one person. When they open it, you '
            'will see them here and decide whether to let them in. The link '
            'works once and stops after seven days.',
          ),
      ];
    }
    final pending = _requests?.pending ?? const <JoinRequest>[];
    final shareLink = inviteLinkForSharing(
      invite.link,
      webOrigin: Uri.base.replace(query: '', fragment: '').toString(),
    );
    return [
      const Text('Send this link to the person you are inviting:'),
      const SizedBox(height: 8),
      Row(
        children: [
          Expanded(
            child: SelectableText(
              shareLink,
              key: const Key('invite-link'),
              style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
            ),
          ),
          IconButton(
            key: const Key('copy-invite-link'),
            icon: const Icon(Icons.copy),
            tooltip: 'Copy the link',
            onPressed: () async {
              await Clipboard.setData(ClipboardData(text: shareLink));
              if (!mounted) return;
              ScaffoldMessenger.maybeOf(
                context,
              )?.showSnackBar(const SnackBar(content: Text('Link copied')));
            },
          ),
        ],
      ),
      const SizedBox(height: 4),
      Text(
        'It works once and stops in ${_days(_requests?.expiresIn ?? invite.expiresIn)}. '
        'It does not contain the note, only a way to ask for it.',
        style: Theme.of(context).textTheme.bodySmall,
      ),
      const SizedBox(height: 12),
      if (pending.isEmpty)
        const Text('Nobody has asked to join yet.', key: Key('invite-waiting')),
    ];
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Add collaborators'),
      content: SizedBox(
        width: 420,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (_directory?.canManage == true &&
                  (_requests?.pending.isNotEmpty ?? false)) ...[
                Text(
                  'Requests to join',
                  style: Theme.of(context).textTheme.titleSmall,
                ),
                for (final request in _requests!.pending) _request(request),
                const Divider(height: 28),
              ],
              ..._people(),
              if (_directory?.canManage == true) ..._content(),
              if (_error != null) ...[
                const SizedBox(height: 12),
                Text(
                  _error!,
                  key: const Key('invite-error'),
                  style: TextStyle(color: Theme.of(context).colorScheme.error),
                ),
              ],
            ],
          ),
        ),
      ),
      actions: [
        if (_directory?.canManage == true)
          TextButton(
            key: const Key('revoke-collaborators'),
            onPressed: _busy ? null : _revoke,
            child: const Text('Remove everyone'),
          ),
        if (_directory?.canManage == true && _invite != null)
          TextButton(
            key: const Key('withdraw-invite'),
            onPressed: _busy ? null : _withdraw,
            child: const Text('Cancel the link'),
          )
        else if (_directory?.canManage == true)
          FilledButton.icon(
            key: const Key('create-invite'),
            onPressed: _busy ? null : _create,
            icon: const Icon(Icons.person_add_outlined),
            label: const Text('Invite by link'),
          ),
        TextButton(
          key: const Key('share-done'),
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Done'),
        ),
      ],
    );
  }
}

/// Asks for an invite link and a name, and pops with the request it made.
class JoinDialog extends StatefulWidget {
  const JoinDialog({super.key, required this.invites, this.initialLink});

  final Invites invites;
  final String? initialLink;

  @override
  State<JoinDialog> createState() => _JoinDialogState();
}

class _JoinDialogState extends State<JoinDialog> {
  late final _link = TextEditingController(text: widget.initialLink);
  final _name = TextEditingController();
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _link.dispose();
    _name.dispose();
    super.dispose();
  }

  Future<void> _ask() async {
    final link = _link.text.trim();
    if (link.isEmpty) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final join = await widget.invites.requestJoin(link, name: _name.text);
      if (mounted) Navigator.of(context).pop(join);
    } catch (e) {
      // A link is pasted by hand, so a bad one is ordinary. Its own words,
      // because "invalid" would not help anyone fix a truncated paste.
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Join a note'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            key: const Key('join-link-field'),
            controller: _link,
            autofocus: true,
            decoration: const InputDecoration(
              labelText: 'Invite link',
              hintText: 'easynotes://join#…',
            ),
          ),
          const SizedBox(height: 8),
          TextField(
            key: const Key('join-name-field'),
            controller: _name,
            maxLength: 32,
            decoration: const InputDecoration(
              labelText: 'Your name',
              helperText: 'Shown to whoever sent the link.',
            ),
            onSubmitted: (_) => _ask(),
          ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _error!,
                key: const Key('join-error'),
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
            ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('join-confirm'),
          onPressed: _busy ? null : _ask,
          child: const Text('Ask to join'),
        ),
      ],
    );
  }
}

/// Waits to be let in, and pops with the note once it is.
///
/// Closing it does not cancel the request — the banner on the list keeps
/// waiting — and "Stop waiting" does.
class WaitingDialog extends StatefulWidget {
  const WaitingDialog({super.key, required this.invites, required this.join});

  final Invites invites;
  final PendingJoin join;

  @override
  State<WaitingDialog> createState() => _WaitingDialogState();
}

class _WaitingDialogState extends State<WaitingDialog> {
  Timer? _poll;
  JoinCheck? _ended;
  bool _checking = false;

  @override
  void initState() {
    super.initState();
    _poll = Timer.periodic(invitePollInterval, (_) => _check());
    _check();
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  Future<void> _check() async {
    if (_checking || _ended != null) return;
    _checking = true;
    try {
      final check = await widget.invites.checkJoin(widget.join.inviteId);
      if (!mounted) return;
      switch (check.state) {
        case JoinState.waiting:
          break;
        case JoinState.joined:
          _poll?.cancel();
          Navigator.of(context).pop(check.noteId);
        case JoinState.declined:
        case JoinState.gone:
          _poll?.cancel();
          setState(() => _ended = check);
      }
    } catch (_) {
      // Offline, or the relay is busy. The next tick asks again.
    } finally {
      _checking = false;
    }
  }

  @override
  Widget build(BuildContext context) {
    final ended = _ended;
    if (ended != null) {
      return AlertDialog(
        title: const Text('Request declined'),
        content: Text(
          ended.state == JoinState.declined
              ? 'The owner turned this request away.'
              : ended.message ?? 'This invite no longer works.',
          key: const Key('join-ended'),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('OK'),
          ),
        ],
      );
    }
    return AlertDialog(
      title: const Text('Waiting for approval'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Text(
            'The person who sent the link will see your request. If they ask, '
            'tell them this code:',
          ),
          const SizedBox(height: 12),
          Center(
            child: Text(
              widget.join.code,
              key: const Key('join-code'),
              style: Theme.of(context).textTheme.headlineMedium,
            ),
          ),
          const SizedBox(height: 12),
          const LinearProgressIndicator(),
        ],
      ),
      actions: [
        TextButton(
          key: const Key('join-stop-waiting'),
          onPressed: () async {
            await widget.invites.cancelJoin(widget.join.inviteId);
            if (context.mounted) Navigator.of(context).pop();
          },
          child: const Text('Stop waiting'),
        ),
        TextButton(
          key: const Key('join-hide'),
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Keep waiting in the background'),
        ),
      ],
    );
  }
}

/// A request somebody made on one of this device's invites.
class _Asking {
  const _Asking(this.invite, this.request);
  final Invite invite;
  final JoinRequest request;
}

/// What sits above the grid while an invite is waiting on either side.
///
/// One line per person asking to join one of this device's notes, with a way
/// to go and answer; one line per note this device has asked to join, with the
/// code and a way to stop. It polls on its own timer, so a person who shared
/// a link and closed the dialog still finds out that someone opened it.
class InviteBanners extends StatefulWidget {
  const InviteBanners({
    super.key,
    required this.invites,
    required this.titleOf,
    required this.onReview,
    required this.onJoined,
  });

  final Invites invites;

  /// A note's title, for "Anna wants to join Groceries".
  final String Function(String noteId) titleOf;

  /// Open the share dialog for a note.
  final void Function(String noteId) onReview;

  /// A request of this device's was let in.
  final void Function(String noteId) onJoined;

  @override
  State<InviteBanners> createState() => _InviteBannersState();
}

class _InviteBannersState extends State<InviteBanners> {
  Timer? _poll;
  List<_Asking> _asking = const [];
  List<PendingJoin> _waiting = const [];
  final Map<String, String> _ended = {};
  bool _checking = false;

  @override
  void initState() {
    super.initState();
    _waiting = widget.invites.pendingJoins;
    _poll = Timer.periodic(inviteBannerPollInterval, (_) => _check());
    _check();
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  Future<void> _check() async {
    if (_checking) return;
    _checking = true;
    try {
      final asking = <_Asking>[];
      for (final invite in widget.invites.allInvites) {
        try {
          final requests = await widget.invites.inviteRequests(invite.id);
          for (final request in requests.pending) {
            if (request.readable) asking.add(_Asking(invite, request));
          }
        } catch (_) {
          // Offline, or gone; either way nobody to show.
        }
      }
      for (final join in widget.invites.pendingJoins) {
        try {
          final check = await widget.invites.checkJoin(join.inviteId);
          switch (check.state) {
            case JoinState.joined:
              if (check.noteId != null) widget.onJoined(check.noteId!);
            case JoinState.declined:
              _ended[join.inviteId] =
                  'Your request to join a note was declined.';
            case JoinState.gone:
              _ended[join.inviteId] =
                  check.message ??
                  'An invite you were waiting on stopped working.';
            case JoinState.waiting:
              break;
          }
        } catch (_) {}
      }
      if (!mounted) return;
      setState(() {
        _asking = asking;
        _waiting = widget.invites.pendingJoins;
      });
    } finally {
      _checking = false;
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final lines = <Widget>[
      for (final asking in _asking)
        _Line(
          key: Key('invite-banner-${asking.request.id}'),
          colour: scheme.primaryContainer,
          text:
              '${asking.request.name.isEmpty ? 'Someone' : asking.request.name} '
              'wants to join ${widget.titleOf(asking.invite.noteId)}',
          action: 'Review',
          onAction: () async {
            widget.onReview(asking.invite.noteId);
          },
        ),
      for (final join in _waiting)
        _Line(
          key: Key('join-banner-${join.inviteId}'),
          colour: scheme.secondaryContainer,
          text: 'Waiting to join a note. Your code is ${join.code}.',
          action: 'Stop waiting',
          onAction: () async {
            await widget.invites.cancelJoin(join.inviteId);
            if (mounted) setState(() => _waiting = widget.invites.pendingJoins);
          },
        ),
      for (final entry in _ended.entries)
        _Line(
          key: Key('join-ended-${entry.key}'),
          colour: scheme.errorContainer,
          text: entry.value,
          action: 'OK',
          onAction: () async => setState(() => _ended.remove(entry.key)),
        ),
    ];
    if (lines.isEmpty) return const SizedBox.shrink();
    return Column(mainAxisSize: MainAxisSize.min, children: lines);
  }
}

class _Line extends StatelessWidget {
  const _Line({
    super.key,
    required this.colour,
    required this.text,
    required this.action,
    required this.onAction,
  });

  final Color colour;
  final String text;
  final String action;
  final Future<void> Function() onAction;

  @override
  Widget build(BuildContext context) {
    return Material(
      color: colour,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
        child: Row(
          children: [
            Expanded(child: Text(text)),
            TextButton(onPressed: onAction, child: Text(action)),
          ],
        ),
      ),
    );
  }
}
