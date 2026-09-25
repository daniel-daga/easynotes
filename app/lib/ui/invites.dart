/// Sharing a note with other people, from both ends.
///
/// The owner's side is [ShareDialog]: send a link through the share sheet,
/// see who opened it, and remove anyone later. The guest's side is
/// [JoinDialog] and [WaitingDialog]: tap the link (or, from the drawer, paste
/// it), join under the name the account shares with, and wait the moment it
/// takes the owner's device to let them in. Nobody approves anything: the
/// link works once, for the first account that opens it. [InviteBanners] is
/// what sits above the grid while either side is waiting, so that closing a
/// dialog does not mean missing the answer — and it is what lets a person in
/// while the owner's share dialog is closed.
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

import '../core/content_sharing.dart';
import '../core/invite_links.dart';
import '../core/invites.dart';
import 'account_screen.dart';

/// How often a screen that is waiting asks the relay. Short, because somebody
/// is looking at it; a variable, so a widget test can make it shorter still.
Duration invitePollInterval = const Duration(seconds: 3);

/// How often the list screen asks, with nobody necessarily looking.
Duration inviteBannerPollInterval = const Duration(seconds: 15);

/// How the owner is told somebody joined when they are not looking at the
/// share dialog. The app sets it to system notifications; unset, nobody is
/// told beyond the line above the grid. A variable, like the intervals, so a
/// widget test can listen.
JoinAlerts? joinAlerts;

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
  InviteStatus? _status;
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
      final status = await widget.invites.checkInvite(invite.id);
      if (!mounted || _invite != invite) return;
      setState(() {
        _status = status;
        _loadPeople();
        if (status.joined != null) {
          _notice = '${status.joined} joined. They can open this note now.';
        }
        if (!status.open && status.lettingIn == null) {
          _invite = null;
          _status = null;
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
          _status = null;
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

  String _link(Invite invite) =>
      inviteLinkForSharing(invite.link, webOrigin: Uri.base.toString());

  /// Send the link through the platform's share sheet, making it first if
  /// there is none open. Where there is no share sheet it is copied instead.
  Future<void> _sendLink() => _act(() async {
    final invite = await _openInvite();
    if (invite == null) return;
    // The link goes last: a guest who pastes the whole message rather than
    // tapping it still gets in, because the core reads after the last `#`.
    final shared = await shareContent(
      'Join my shared note in EasyNotes: ${_link(invite)}',
    );
    if (!shared) _copy(invite);
    await _refresh();
  });

  /// Copy the link, making it first if there is none open.
  Future<void> _copyLink() => _act(() async {
    final invite = await _openInvite();
    if (invite == null) return;
    _copy(invite);
    await _refresh();
  });

  Future<Invite?> _openInvite() async {
    if (_invite == null) {
      // A new link is when to ask about notifications: the owner is about to
      // wait for somebody, and will want to hear when they arrive. Not
      // awaited, so the link goes whatever the answer.
      unawaited(joinAlerts?.requestPermission().catchError((_) {}));
    }
    final invite = _invite ?? await widget.invites.createInvite(widget.noteId);
    if (!mounted) return null;
    setState(() {
      _invite = invite;
      _notice = null;
    });
    return invite;
  }

  void _copy(Invite invite) {
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
  }

  Future<void> _withdraw() => _act(() async {
    await widget.invites.withdrawInvite(_invite!.id);
    if (!mounted) return;
    setState(() {
      _invite = null;
      _status = null;
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
        _status = null;
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

  @override
  Widget build(BuildContext context) {
    final people = _people;
    final canManage = people?.canManage ?? false;
    final invite = _invite;
    final lettingIn = _status?.lettingIn;
    return AlertDialog(
      title: const Text('Share'),
      content: SizedBox(
        width: 420,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
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
                    'Send a link to one person, any way you like. Whoever '
                    'opens it first joins the note, and you will see them '
                    'here. The link works once and stops after seven days.',
                  )
                else ...[
                  Row(
                    children: [
                      Expanded(
                        child: SelectableText(
                          _link(invite),
                          key: const Key('invite-link'),
                          style: const TextStyle(
                            fontFamily: 'monospace',
                            fontSize: 12,
                          ),
                        ),
                      ),
                      IconButton(
                        key: const Key('copy-invite-link'),
                        icon: const Icon(Icons.copy),
                        tooltip: 'Copy link',
                        onPressed: _busy ? null : _copyLink,
                      ),
                    ],
                  ),
                  const SizedBox(height: 4),
                  Text(
                    'It works once, for whoever opens it first, and stops in '
                    '${_days(_status?.expiresIn ?? invite.expiresIn)}. It '
                    'does not contain the note, only a way in.',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  if (lettingIn == null) ...[
                    const SizedBox(height: 8),
                    const Text(
                      'Nobody has opened it yet.',
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
            key: const Key('send-invite-link'),
            onPressed: _busy ? null : _sendLink,
            icon: const Icon(Icons.send),
            label: const Text('Send link'),
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

/// Joins by an invite link, and pops with the request it made. A link that
/// was tapped is only confirmed; the drawer's way in asks for one to be
/// pasted. The name is the account's: the button says it, and there is no
/// field for it.
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
    // A tapped link needs only a yes: showing its secret in a field to be
    // checked would ask the guest to judge something they cannot read.
    final tapped = widget.initialLink != null;
    return AlertDialog(
      title: Text(tapped ? 'Join a shared note?' : 'Join with a link'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (tapped)
            const Text(
              'You will be added to the note, and the person who sent the '
              'link will see you there. The link works for one person only.',
              key: Key('join-explainer'),
            )
          else
            TextField(
              key: const Key('join-link-field'),
              controller: _link,
              autofocus: true,
              decoration: const InputDecoration(
                labelText: 'Invite link',
                hintText: 'Paste the link you were sent',
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
          child: Text(name.isEmpty ? 'Join' : 'Join as $name'),
        ),
      ],
    );
  }
}

/// Waits for the owner's device to let this person in, and pops with the note
/// once it opens.
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
        title: Text(
          ended.state == JoinState.declined
              ? 'Could not join'
              : 'This link stopped working',
        ),
        content: Text(
          ended.state == JoinState.declined
              ? 'The owner\'s device could not read this request. Ask them '
                    'for a new link.'
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
      title: Text(_opening ? 'Opening the note' : 'Joining'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (_opening)
            const Text(
              'You are in. The note is on its way to this device.',
              key: Key('join-opening'),
            )
          else
            const Text(
              'You will be let in as soon as the person who sent the link '
              'has EasyNotes open.',
              key: Key('join-waiting'),
            ),
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

/// Somebody who opened one of this device's links and is now in.
class _Joined {
  const _Joined(this.noteId, this.name);
  final String noteId;
  final String name;
}

/// What sits above the grid while an invite is waiting on either side.
///
/// It checks this device's open invites on its own timer, which is what lets
/// a person in when the owner shared a link and closed the dialog — and
/// finishes letting someone in when the app closed half-way. One line per
/// person who got in that way, with a way to see everyone on the note, and a
/// system notification through [joinAlerts] for an owner who is not looking;
/// one line per note this device is waiting to join, with a way to stop.
class InviteBanners extends StatefulWidget {
  const InviteBanners({
    super.key,
    required this.invites,
    required this.titleOf,
    required this.onReview,
    required this.onJoined,
  });

  final Invites invites;

  /// A note's title, for "Anna joined Groceries".
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
  final Map<String, _Joined> _joined = {};
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
      final joined = <String, _Joined>{};
      for (final invite in widget.invites.allInvites) {
        try {
          final status = await widget.invites.checkInvite(invite.id);
          if (status.joined != null) {
            joined[invite.id] = _Joined(invite.noteId, status.joined!);
            // Once: the invite is forgotten as it reports who joined. The
            // share dialog, if it caught the join first, says so itself.
            unawaited(
              joinAlerts
                  ?.joined(
                    inviteId: invite.id,
                    noteId: invite.noteId,
                    name: status.joined!,
                  )
                  .catchError((_) {}),
            );
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
                  'A link you opened could not be used. Ask for a new one.';
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
        _joined.addAll(joined);
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
      for (final entry in _joined.entries)
        _Line(
          key: Key('invite-banner-${entry.key}'),
          colour: scheme.primaryContainer,
          text:
              '${entry.value.name.isEmpty ? 'Someone' : entry.value.name} '
              'joined ${widget.titleOf(entry.value.noteId)}',
          action: 'View',
          onAction: () async {
            setState(() => _joined.remove(entry.key));
            widget.onReview(entry.value.noteId);
          },
        ),
      for (final join in _waiting)
        _Line(
          key: Key('join-banner-${join.inviteId}'),
          colour: scheme.secondaryContainer,
          text: 'Joining a shared note…',
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
