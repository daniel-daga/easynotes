/// Sharing a note with other people, from both ends.
///
/// The owner's side is [ShareDialog]: copy a link, send it however they like,
/// approve whoever asks with one tap, and remove anyone later. The guest's
/// side is [JoinDialog] and [WaitingDialog]: paste the link, ask to join under
/// the name the account shares with, and wait to be let in. [InviteBanners] is
/// what sits above the grid while either side is waiting, so that closing a
/// dialog does not mean missing the answer.
///
/// Sharing needs an account — shared notes belong to it, which is how they
/// come back on a new phone — so the first time either side starts,
/// [ensureSharingSetUp] asks for one, and a name, and carries straight on.
///
/// All of it polls. There is no push channel yet, and a person waiting on
/// someone else is looking at the screen anyway. The polls stop when the
/// widget goes.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/invite_links.dart';
import '../core/invites.dart';
import 'account_screen.dart';

/// How often a screen that is waiting asks the relay. Short, because somebody
/// is looking at it; a variable, so a widget test can make it shorter still.
Duration invitePollInterval = const Duration(seconds: 3);

/// How often the list screen asks, with nobody necessarily looking.
Duration inviteBannerPollInterval = const Duration(seconds: 15);

/// Rounded to the nearest unit, not down: a link made a second ago has
/// 6 d 23 h 59 m left, and the dialog that just said "seven days" should not
/// follow it with "6 days".
String _days(Duration left) {
  if (left >= const Duration(hours: 36)) {
    return '${(left.inMinutes / Duration.minutesPerDay).round()} days';
  }
  if (left >= const Duration(minutes: 90)) {
    return '${(left.inMinutes / Duration.minutesPerHour).round()} hours';
  }
  if (left >= const Duration(seconds: 90)) {
    return '${(left.inSeconds / Duration.secondsPerMinute).round()} minutes';
  }
  return 'a moment';
}

enum _Setup { create, restore }

/// Make sure this device can share: an account, then a name. Asked once, and
/// only when somebody taps Share or opens a link; `false` if they said not
/// now.
///
/// [ask] picks the backup check's words, for a test.
Future<bool> ensureSharingSetUp(
  BuildContext context,
  Accounts accounts, {
  List<int>? ask,
}) async {
  if (!accounts.hasAccount) {
    final choice = await showModalBottomSheet<_Setup>(
      context: context,
      isScrollControlled: true,
      builder: (context) => const SharingSetupSheet(),
    );
    if (choice == null || !context.mounted) return false;
    switch (choice) {
      case _Setup.create:
        final List<String> words;
        try {
          words = await accounts.createAccount();
        } catch (e) {
          if (context.mounted) {
            ScaffoldMessenger.maybeOf(
              context,
            )?.showSnackBar(SnackBar(content: Text('$e')));
          }
          return false;
        }
        if (!context.mounted) return false;
        await Navigator.of(context).push(
          MaterialPageRoute<void>(
            builder: (context) => RecoveryPhraseScreen(words: words, ask: ask),
          ),
        );
      case _Setup.restore:
        final adopted = await Navigator.of(context).push<bool>(
          MaterialPageRoute<bool>(
            builder: (context) => AdoptAccountScreen(library: accounts),
          ),
        );
        if (adopted != true) return false;
    }
    if (!context.mounted) return false;
  }
  if (accounts.profileName == null) {
    final name = await showDialog<String>(
      context: context,
      builder: (context) => const NameDialog(),
    );
    if (name == null) return false;
    await accounts.setProfileName(name);
  }
  return true;
}

/// The one sheet a device without an account sees before it shares or joins.
class SharingSetupSheet extends StatelessWidget {
  const SharingSetupSheet({super.key});

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(24, 24, 24, 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              'Set up sharing',
              key: const Key('setup-sharing-title'),
              style: Theme.of(context).textTheme.titleLarge,
            ),
            const SizedBox(height: 12),
            const Text(
              'Shared notes belong to your account, so they come back if you '
              'get a new phone. Your account is twelve words you write down '
              'once.',
            ),
            const SizedBox(height: 24),
            FilledButton(
              key: const Key('setup-sharing'),
              onPressed: () => Navigator.of(context).pop(_Setup.create),
              child: const Text('Set up'),
            ),
            const SizedBox(height: 8),
            OutlinedButton(
              key: const Key('setup-restore'),
              onPressed: () => Navigator.of(context).pop(_Setup.restore),
              child: const Text('I already have twelve words'),
            ),
            const SizedBox(height: 8),
            TextButton(
              key: const Key('setup-not-now'),
              onPressed: () => Navigator.of(context).pop(),
              child: const Text('Not now'),
            ),
          ],
        ),
      ),
    );
  }
}

/// Everybody on a note, and everything its owner does about who is.
class ShareDialog extends StatefulWidget {
  const ShareDialog({super.key, required this.invites, required this.noteId});

  final Invites invites;
  final String noteId;

  @override
  State<ShareDialog> createState() => _ShareDialogState();
}

class _ShareDialogState extends State<ShareDialog> {
  People? _people;
  Invite? _invite;
  InviteRequests? _requests;
  String? _error;

  /// What the last gesture came to: “Anna can open this note now.”
  String? _notice;
  bool _busy = false;
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    _loadPeople();
    // The newest open invite, if this note has one: a person who closed the
    // dialog and came back should find the link they sent, not a new one.
    final open = widget.invites.invitesFor(widget.noteId);
    if (open.isNotEmpty) _invite = open.last;
    _poll = Timer.periodic(invitePollInterval, (_) => _tick());
    _refresh();
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  void _loadPeople() {
    try {
      _people = widget.invites.people(widget.noteId);
    } catch (e) {
      _error = '$e';
    }
  }

  void _tick() {
    if (!mounted) return;
    setState(_loadPeople);
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
        _loadPeople();
        if (!requests.open && requests.lettingIn == null) {
          _invite = null;
          _requests = null;
        }
      });
    } catch (e) {
      if (!mounted || _invite != invite) return;
      // Offline is the ordinary case and the next tick will try again; an
      // invite the relay has lost is not, and the core has forgotten it.
      setState(() {
        _error = '$e';
        if (widget.invites
            .invitesFor(widget.noteId)
            .every((i) => i.id != invite.id)) {
          _invite = null;
          _requests = null;
        }
      });
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
          _loadPeople();
        });
      }
    }
  }

  String _link(Invite invite) => inviteLinkForSharing(
    invite.link,
    webOrigin: Uri.base.replace(query: '', fragment: '').toString(),
  );

  /// Copy the link, making it first if there is none open.
  Future<void> _copyLink() => _act(() async {
    final invite = _invite ?? await widget.invites.createInvite(widget.noteId);
    if (!mounted) return;
    setState(() {
      _invite = invite;
      _notice = null;
    });
    final messenger = ScaffoldMessenger.maybeOf(context);
    // The link is on screen whatever the clipboard does: a platform without
    // one still lets it be copied by hand.
    unawaited(
      Clipboard.setData(ClipboardData(text: _link(invite))).then(
        (_) => messenger?.showSnackBar(
          const SnackBar(content: Text('Link copied')),
        ),
        onError: (_) => null,
      ),
    );
    await _refresh();
  });

  Future<void> _approve(JoinRequest request) => _act(() async {
    final approval = await widget.invites.approveRequest(
      _invite!.id,
      request.id,
    );
    if (!mounted) return;
    final name = request.name.isEmpty ? 'They' : request.name;
    setState(() {
      _notice = switch (approval) {
        Approval.done => '$name can open this note now.',
        Approval.pending => 'Letting $name in… finishes when you\'re online.',
        Approval.alreadyMember => '$name already has access. Nothing changed.',
      };
      if (approval != Approval.pending) {
        _invite = null;
        _requests = null;
      }
    });
  });

  Future<void> _decline(JoinRequest request) => _act(() async {
    await widget.invites.declineRequest(_invite!.id, request.id);
    await _refresh();
  });

  Future<void> _withdraw() => _act(() async {
    await widget.invites.withdrawInvite(_invite!.id);
    if (!mounted) return;
    setState(() {
      _invite = null;
      _requests = null;
    });
  });

  Future<bool> _confirm({
    required String title,
    required String body,
    required String action,
    required String keyName,
  }) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title),
        content: Text(body),
        actions: [
          TextButton(
            key: Key('cancel-$keyName'),
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            key: Key('confirm-$keyName'),
            onPressed: () => Navigator.of(context).pop(true),
            child: Text(action),
          ),
        ],
      ),
    );
    return confirmed == true && mounted;
  }

  Future<void> _remove(Person person) async {
    final name = person.displayName;
    if (!await _confirm(
      title: 'Remove $name?',
      body:
          '$name will lose this note on every device. Anything $name already '
          'saved or copied can\'t be taken back.',
      action: 'Remove',
      keyName: 'remove',
    )) {
      return;
    }
    await _act(() => widget.invites.removePerson(widget.noteId, person));
  }

  Future<void> _stopSharing() async {
    if (!await _confirm(
      title: 'Stop sharing?',
      body:
          'Everyone else will lose this note on every device. Anything they '
          'already saved or copied can\'t be taken back.',
      action: 'Stop sharing',
      keyName: 'stop-sharing',
    )) {
      return;
    }
    await _act(() async {
      await widget.invites.stopSharing(widget.noteId);
      if (!mounted) return;
      setState(() {
        _invite = null;
        _requests = null;
        _notice = null;
      });
    });
  }

  Widget _person(Person person, bool canManage) {
    final subtitle = person.removing
        ? 'Removing… finishes when you\'re online'
        : person.owner
        ? 'Owner'
        : null;
    return ListTile(
      key: Key('person-${person.member}'),
      contentPadding: EdgeInsets.zero,
      leading: CircleAvatar(child: Text(person.initials)),
      title: Text('${person.displayName}${person.me ? ' (you)' : ''}'),
      subtitle: subtitle == null
          ? null
          : Text(subtitle, key: Key('person-status-${person.member}')),
      trailing: canManage && !person.owner && !person.removing
          ? TextButton(
              key: Key('remove-${person.member}'),
              onPressed: _busy ? null : () => _remove(person),
              child: const Text('Remove'),
            )
          : null,
    );
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
              request.alreadyMember
                  ? '$name already has access'
                  : '$name wants to join',
              style: Theme.of(context).textTheme.titleSmall,
            ),
            const SizedBox(height: 4),
            if (!request.readable)
              const Text('This request was not made with your link.')
            else if (request.alreadyMember)
              const Text('Approving changes nothing.')
            else
              Text(
                'Their screen shows ${request.code}.',
                key: Key('join-code-${request.id}'),
              ),
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

  @override
  Widget build(BuildContext context) {
    final people = _people;
    final canManage = people?.canManage ?? false;
    final invite = _invite;
    final pending = _requests?.pending ?? const <JoinRequest>[];
    final lettingIn = _requests?.lettingIn;
    return AlertDialog(
      title: const Text('Share'),
      content: SizedBox(
        width: 420,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (canManage && pending.isNotEmpty) ...[
                for (final request in pending) _request(request),
                const Divider(height: 28),
              ],
              if (people == null)
                const Text('Loading…')
              else ...[
                for (final person in people.people) _person(person, canManage),
                for (final name in {...people.adding, ?lettingIn})
                  ListTile(
                    key: Key('adding-$name'),
                    contentPadding: EdgeInsets.zero,
                    leading: const CircleAvatar(
                      child: Icon(Icons.hourglass_top),
                    ),
                    title: Text(name),
                    subtitle: const Text(
                      'Letting them in… finishes when you\'re online',
                    ),
                  ),
                const SizedBox(height: 4),
                Text(
                  'Names are chosen by each person.',
                  key: const Key('people-footer'),
                  style: Theme.of(context).textTheme.bodySmall,
                ),
                if (!canManage) ...[
                  const SizedBox(height: 12),
                  const Text('Only the owner can add or remove people.'),
                ],
              ],
              if (canManage) ...[
                const Divider(height: 28),
                if (invite == null)
                  const Text(
                    'Copy a link and send it to one person, any way you like. '
                    'When they open it you will see them here, and approve '
                    'them with one tap. The link works once and stops after '
                    'seven days.',
                  )
                else ...[
                  SelectableText(
                    _link(invite),
                    key: const Key('invite-link'),
                    style: const TextStyle(
                      fontFamily: 'monospace',
                      fontSize: 12,
                    ),
                  ),
                  const SizedBox(height: 4),
                  Text(
                    'It works once and stops in '
                    '${_days(_requests?.expiresIn ?? invite.expiresIn)}. It '
                    'does not contain the note, only a way to ask for it.',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  if (pending.isEmpty && lettingIn == null) ...[
                    const SizedBox(height: 8),
                    const Text(
                      'Nobody has asked to join yet.',
                      key: Key('invite-waiting'),
                    ),
                  ],
                ],
              ],
              if (_notice != null) ...[
                const SizedBox(height: 12),
                Text(_notice!, key: const Key('share-notice')),
              ],
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
        if (canManage && (people?.shared ?? false))
          TextButton(
            key: const Key('stop-sharing'),
            onPressed: _busy ? null : _stopSharing,
            child: const Text('Stop sharing'),
          ),
        if (canManage && invite != null)
          TextButton(
            key: const Key('withdraw-invite'),
            onPressed: _busy ? null : _withdraw,
            child: const Text('Cancel the link'),
          ),
        if (canManage)
          FilledButton.icon(
            key: const Key('copy-invite-link'),
            onPressed: _busy ? null : _copyLink,
            icon: const Icon(Icons.link),
            label: const Text('Copy link'),
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

/// Asks for an invite link, and pops with the request it made. The name is
/// the account's: the button says it, and there is no field for it.
class JoinDialog extends StatefulWidget {
  const JoinDialog({super.key, required this.invites, this.initialLink});

  final Invites invites;
  final String? initialLink;

  @override
  State<JoinDialog> createState() => _JoinDialogState();
}

class _JoinDialogState extends State<JoinDialog> {
  late final _link = TextEditingController(text: widget.initialLink);
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _link.dispose();
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
      final join = await widget.invites.requestJoin(link);
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
    final name = widget.invites.profileName ?? '';
    return AlertDialog(
      title: const Text('Join a note'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            key: const Key('join-link-field'),
            controller: _link,
            autofocus: widget.initialLink == null,
            decoration: const InputDecoration(
              labelText: 'Invite link',
              hintText: 'easynotes://join#…',
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
          child: Text(name.isEmpty ? 'Ask to join' : 'Ask to join as $name'),
        ),
      ],
    );
  }
}

/// Waits to be let in, and pops with the note once it opens.
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
  bool _opening = false;
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
        case JoinState.opening:
          setState(() => _opening = true);
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
      title: Text(_opening ? 'Opening the note' : 'Waiting for approval'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (_opening)
            const Text(
              'You are in. The note is on its way to this device.',
              key: Key('join-opening'),
            )
          else ...[
            const Text(
              'The person who sent the link will see your request. If they '
              'ask, tell them this code:',
            ),
            const SizedBox(height: 12),
            Center(
              child: Text(
                widget.join.code,
                key: const Key('join-code'),
                style: Theme.of(context).textTheme.headlineMedium,
              ),
            ),
          ],
          const SizedBox(height: 12),
          const LinearProgressIndicator(),
        ],
      ),
      actions: [
        if (!_opening)
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
/// a link and closed the dialog still finds out that someone opened it — and
/// an approval interrupted by the app closing finishes here.
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

  /// A request of this device's was let in and its note has opened.
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
            case JoinState.opening:
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
