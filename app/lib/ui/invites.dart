/// Sharing a note with other people, from both ends.
///
/// The owner's side is [CollaboratorsScreen]: a table of the people on the
/// note and a table of the invite links still open to it. Open as many links
/// as there are people to invite, send each through the share sheet, see who
/// opened which, and remove anyone later. The guest's side is
/// [JoinDialog] and [WaitingDialog]: tap the link (or, from the drawer, paste
/// it), join under the name the account shares with, and wait the moment it
/// takes the owner's device to let them in. Nobody approves anything: the
/// link works once, for the first account that opens it. [InviteBanners] is
/// what sits above the grid while either side is waiting, so that closing a
/// dialog does not mean missing the answer — and it is what lets a person in
/// while the owner's collaborators screen is closed.
///
/// Sharing needs a sync server and the account that belongs to it — shared
/// notes belong to that account, which is how they come back on a new phone.
/// A device without them is sent to Sync & devices rather than offered a
/// second, local account.
///
/// All of it polls. There is no push channel yet, and a person waiting on
/// someone else is looking at the screen anyway. The polls stop when the
/// widget goes.
library;

import 'dart:async';

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../core/content_sharing.dart';
import '../core/app_visibility.dart';
import '../core/invite_links.dart';
import '../core/relay_profiles.dart';
import '../core/invites.dart';
import 'account_screen.dart' show NameDialog;

/// How often a screen that is waiting asks the relay. Short, because somebody
/// is looking at it; a variable, so a widget test can make it shorter still.
Duration invitePollInterval = const Duration(seconds: 3);

/// How often the list screen asks, with nobody necessarily looking.
Duration inviteBannerPollInterval = const Duration(seconds: 15);

/// How the owner is told somebody joined when they are not looking at the
/// collaborators screen. The app sets it to system notifications; unset,
/// nobody is told beyond the line above the grid. A variable, like the
/// intervals, so a widget test can listen.
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

/// Make sure this device can share: it needs a sync server and the account
/// that belongs to it (ADR 0031), so there is no account-only path here.
/// `false` if the device is not set up; the sheet says where to do it.
Future<bool> ensureSharingSetUp(
  BuildContext context,
  Accounts accounts, {
  List<int>? ask,
  bool joining = false,
}) async {
  if (!accounts.hasAccount) {
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      builder: (context) => SharingSetupSheet(joining: joining),
    );
    return false;
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

/// The one sheet a device without sync sees before it shares or joins.
///
/// [joining] is the second person's way in: they did not come here to set up
/// sync, they came to open a note. The sheet says what to do next without
/// explaining where the note is kept.
class SharingSetupSheet extends StatelessWidget {
  const SharingSetupSheet({super.key, this.joining = false});

  final bool joining;

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
              joining ? 'One step before you join' : 'Set up sync first',
              key: const Key('setup-sharing-title'),
              style: Theme.of(context).textTheme.titleLarge,
            ),
            const SizedBox(height: 12),
            Text(
              joining
                  ? 'This note is kept where you can both open it. Set that '
                        'up in Sync & devices, then open the link again. It '
                        'takes a minute.'
                  : 'Sharing needs a sync server and the account that belongs '
                        'to it. Set both up together in Sync & devices, write '
                        'down your twelve words, then come back here.',
            ),
            const SizedBox(height: 24),
            FilledButton(
              key: const Key('setup-not-now'),
              onPressed: () => Navigator.of(context).pop(),
              child: const Text('OK'),
            ),
          ],
        ),
      ),
    );
  }
}

/// Open [CollaboratorsScreen] for a note, over whatever is showing.
Future<void> showCollaborators(
  BuildContext context, {
  required Invites invites,
  required String noteId,
  String? relayUrl,
  String? relayAccess,
}) => Navigator.of(context).push(
  MaterialPageRoute<void>(
    fullscreenDialog: true,
    builder: (context) => CollaboratorsScreen(
      invites: invites,
      noteId: noteId,
      relayUrl: relayUrl,
      relayAccess: relayAccess,
    ),
  ),
);

/// Everybody on a note, every link that is still open to it, and everything
/// its owner does about either.
///
/// Two tables: the people who are in, and the invite links on their way to
/// somebody. A link lets one person in, so inviting three people is three
/// links, and the owner can open as many as they like and follow each one on
/// its own row.
class CollaboratorsScreen extends StatefulWidget {
  const CollaboratorsScreen({
    super.key,
    required this.invites,
    required this.noteId,
    this.relayUrl,
    this.relayAccess,
  });

  final Invites invites;
  final String noteId;

  /// The relay this note lives on. Every invite made here carries it, so a
  /// guest who taps one needs nothing typed in first.
  final String? relayUrl;

  /// That relay's access key, when its operator gave this device one
  /// ([ADR 0028](../../../docs/decisions.md#adr-0028)). It
  /// travels in the invite, which is single-use and expires — narrower than
  /// the relay link, which lets whoever holds it use the relay for good.
  final String? relayAccess;

  @override
  State<CollaboratorsScreen> createState() => _CollaboratorsScreenState();
}

class _CollaboratorsScreenState extends State<CollaboratorsScreen> {
  People? _people;

  /// The note's open invites, oldest first, as the core has them.
  List<Invite> _open = const [];

  /// What the relay last said about each of [_open], by invite ID.
  final Map<String, InviteStatus> _status = {};
  String? _error;

  /// What the last gesture came to: “Anna can open this note now.”
  String? _notice;
  bool _busy = false;
  bool _refreshing = false;
  bool _askedToNotify = false;
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    _load();
    _poll = Timer.periodic(invitePollInterval, (_) => _tick());
    _refresh();
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  void _load() {
    try {
      _people = widget.invites.people(widget.noteId);
      _open = widget.invites.invitesFor(widget.noteId);
      _status.removeWhere((id, _) => _open.every((i) => i.id != id));
    } catch (e) {
      _error = '$e';
    }
  }

  void _tick() {
    if (!mounted || !appIsForeground.value) return;
    setState(_load);
    _refresh();
  }

  /// Ask the relay about every open link, one after the other. Whoever has
  /// opened one is let in as a side effect, and drops off the invite table
  /// onto the people table.
  Future<void> _refresh() async {
    if (_refreshing || !appIsForeground.value) return;
    _refreshing = true;
    try {
      for (final invite in List.of(_open)) {
        try {
          final status = await widget.invites.checkInvite(invite.id);
          if (!mounted) return;
          setState(() {
            _status[invite.id] = status;
            if (status.joined != null) {
              _notice = '${status.joined} joined. They can open this note now.';
            }
            _load();
          });
        } catch (e) {
          if (!mounted) return;
          // Offline is the ordinary case and the next tick will try again; an
          // invite the relay has lost is not, and the core has forgotten it.
          setState(() {
            _error = '$e';
            _load();
          });
        }
      }
    } finally {
      _refreshing = false;
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
          _load();
        });
      }
    }
  }

  String _link(Invite invite) {
    final url = widget.relayUrl;
    final relay = url == null ? null : (url: url, access: widget.relayAccess);
    if (!kIsWeb && url != null) {
      // The relay is the link's base, so its own page needs only the secret
      // after the `#`. Tapping it hands the fragment to the app, which is told
      // the relay by the page; a pasted link is read the same way.
      return inviteLinkForRelayPage(invite.link, url);
    }
    // On web the link opens this app, which handles the fragment itself. With
    // no relay there is nothing to page to and the custom scheme stays.
    return inviteLinkForSharing(
      invite.link,
      webOrigin: Uri.base.toString(),
      relay: relay,
    );
  }

  /// Open one more link and send it through the platform's share sheet. Where
  /// there is no share sheet it is copied instead. Every tap is a new link,
  /// for a new person; the ones already open stay open.
  Future<void> _newInvite() => _act(() async {
    if (!_askedToNotify) {
      // A new link is when to ask about notifications: the owner is about to
      // wait for somebody, and will want to hear when they arrive. Not
      // awaited, so the link goes whatever the answer.
      _askedToNotify = true;
      unawaited(joinAlerts?.requestPermission().catchError((_) {}));
    }
    final invite = await widget.invites.createInvite(widget.noteId);
    if (!mounted) return;
    setState(() {
      _load();
      _notice = null;
    });
    await _send(invite);
  });

  Future<void> _send(Invite invite) async {
    // One link, and everything needed to join: the note, the sync server it
    // lives on, and the key that server asks for. Somebody who taps it is in;
    // somebody who pastes the whole message rather than tapping it still is,
    // because the core reads after the last `#`.
    final shared = await shareContent(
      'Open the note I shared with you in EasyNotes: ${_link(invite)}',
    );
    if (!shared && mounted) _copy(invite);
    await _refresh();
  }

  void _copyRelayLink(String link) {
    final messenger = ScaffoldMessenger.maybeOf(context);
    unawaited(
      Clipboard.setData(ClipboardData(text: link)).then(
        (_) => messenger?.showSnackBar(
          const SnackBar(content: Text('Server link copied')),
        ),
        onError: (_) => null,
      ),
    );
  }

  /// What a guest should know about the server this link carries, and about
  /// the server link that shares the same key without an invite.
  Widget _relayLinkSection(ThemeData theme, String link) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      const SizedBox(height: 8),
      Text(
        'This server has an access key, and every invite carries it so one '
        'tap is enough. It is a single-use link that expires, but it does let '
        'its first opener use this server, so send it only to people you '
        'trust with that.',
        key: const Key('relay-link-explainer'),
        style: theme.textTheme.bodySmall,
      ),
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton.icon(
          key: const Key('copy-relay-link'),
          onPressed: () => _copyRelayLink(link),
          icon: const Icon(Icons.key),
          label: const Text('Copy server link'),
        ),
      ),
    ],
  );

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

  Future<void> _withdraw(Invite invite) => _act(() async {
    await widget.invites.withdrawInvite(invite.id);
    _status.remove(invite.id);
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
          'Everyone else will lose this note on every device, and every open '
          'link stops working. Anything they already saved or copied can\'t '
          'be taken back.',
      action: 'Stop sharing',
      keyName: 'stop-sharing',
    )) {
      return;
    }
    await _act(() async {
      await widget.invites.stopSharing(widget.noteId);
      if (!mounted) return;
      setState(() {
        _status.clear();
        _notice = null;
      });
    });
  }

  /// A phone held upright: the tables cap their widest cells and fold what
  /// they can, so that nothing has to be scrolled sideways to be seen.
  bool get _narrow => MediaQuery.sizeOf(context).width < 600;

  /// A cell no wider than [narrow] on a phone and [wide] elsewhere.
  Widget _capped(double narrow, double wide, Widget child) => ConstrainedBox(
    constraints: BoxConstraints(maxWidth: _narrow ? narrow : wide),
    child: child,
  );

  /// A table as wide as the screen, or wider and scrolled sideways on a
  /// screen that still cannot fit it.
  Widget _table({
    required Key key,
    required List<String> columns,
    required List<DataRow> rows,
  }) {
    return LayoutBuilder(
      builder: (context, constraints) => SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: ConstrainedBox(
          constraints: BoxConstraints(minWidth: constraints.maxWidth),
          child: DataTable(
            key: key,
            horizontalMargin: 0,
            columnSpacing: _narrow ? 12 : 24,
            dataRowMinHeight: 56,
            dataRowMaxHeight: 88,
            columns: [
              for (final column in columns) DataColumn(label: Text(column)),
            ],
            rows: rows,
          ),
        ),
      ),
    );
  }

  /// A section's title, with its one action beside it — or under it, where
  /// the two do not fit on a line.
  Widget _heading(String text, {Key? key, Widget? trailing}) {
    return Padding(
      padding: const EdgeInsets.only(top: 8, bottom: 4),
      child: Wrap(
        alignment: WrapAlignment.spaceBetween,
        crossAxisAlignment: WrapCrossAlignment.center,
        spacing: 16,
        runSpacing: 8,
        children: [
          Text(text, key: key, style: Theme.of(context).textTheme.titleMedium),
          ?trailing,
        ],
      ),
    );
  }

  DataRow _personRow(Person person, bool canManage) {
    final role = person.removing
        ? 'Removing… finishes when you\'re online'
        : person.owner
        ? 'Owner'
        : 'Can edit';
    return DataRow(
      cells: [
        DataCell(
          Row(
            key: Key('person-${person.member}'),
            mainAxisSize: MainAxisSize.min,
            children: [
              CircleAvatar(radius: 16, child: Text(person.initials)),
              const SizedBox(width: 12),
              _capped(
                76,
                320,
                Text(
                  '${person.displayName}${person.me ? ' (you)' : ''}',
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
            ],
          ),
        ),
        DataCell(
          _capped(
            96,
            280,
            Text(
              role,
              key: Key('person-status-${person.member}'),
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
            ),
          ),
        ),
        DataCell(
          canManage && !person.owner && !person.removing
              ? TextButton(
                  key: Key('remove-${person.member}'),
                  style: TextButton.styleFrom(
                    visualDensity: VisualDensity.compact,
                  ),
                  onPressed: _busy ? null : () => _remove(person),
                  child: const Text('Remove'),
                )
              : const SizedBox.shrink(),
        ),
      ],
    );
  }

  DataRow _addingRow(String name) => DataRow(
    cells: [
      DataCell(
        Row(
          key: Key('adding-$name'),
          mainAxisSize: MainAxisSize.min,
          children: [
            const CircleAvatar(radius: 16, child: Icon(Icons.hourglass_top)),
            const SizedBox(width: 12),
            _capped(
              76,
              320,
              Text(name, maxLines: 2, overflow: TextOverflow.ellipsis),
            ),
          ],
        ),
      ),
      DataCell(
        _capped(
          96,
          280,
          const Text(
            'Letting them in… finishes when you\'re online',
            maxLines: 3,
            overflow: TextOverflow.ellipsis,
          ),
        ),
      ),
      const DataCell(SizedBox.shrink()),
    ],
  );

  DataRow _inviteRow(int index, Invite invite) {
    final status = _status[invite.id];
    final lettingIn = status?.lettingIn;
    final expiresIn = status?.expiresIn ?? invite.expiresIn;
    return DataRow(
      cells: [
        DataCell(
          _capped(
            64,
            240,
            Tooltip(
              message: _link(invite),
              child: Text(
                _link(invite),
                key: Key('invite-link-${invite.id}'),
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
              ),
            ),
          ),
        ),
        DataCell(
          _capped(
            110,
            280,
            Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  lettingIn == null
                      ? 'Not opened yet'
                      : 'Letting $lettingIn in…',
                  key: Key('invite-status-${invite.id}'),
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                ),
                Text(
                  'Expires in ${_days(expiresIn)}',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ],
            ),
          ),
        ),
        DataCell(
          Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              IconButton(
                key: Key('send-invite-${invite.id}'),
                visualDensity: VisualDensity.compact,
                icon: const Icon(Icons.send),
                tooltip: 'Send link ${index + 1} again',
                onPressed: _busy ? null : () => _act(() => _send(invite)),
              ),
              IconButton(
                key: Key('copy-invite-${invite.id}'),
                visualDensity: VisualDensity.compact,
                icon: const Icon(Icons.copy),
                tooltip: 'Copy link ${index + 1}',
                onPressed: _busy ? null : () => _copy(invite),
              ),
              IconButton(
                key: Key('withdraw-invite-${invite.id}'),
                visualDensity: VisualDensity.compact,
                icon: const Icon(Icons.link_off),
                tooltip: 'Cancel link ${index + 1}',
                onPressed: _busy ? null : () => _withdraw(invite),
              ),
            ],
          ),
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final people = _people;
    final canManage = people?.canManage ?? false;
    final theme = Theme.of(context);
    final lettingIn = {
      for (final status in _status.values)
        if (status.lettingIn != null) status.lettingIn!,
    };
    return Scaffold(
      appBar: AppBar(
        automaticallyImplyLeading: false,
        title: const Text('Collaborators'),
        actions: [
          TextButton(
            key: const Key('share-done'),
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('Done'),
          ),
          const SizedBox(width: 8),
        ],
      ),
      body: SafeArea(
        // Not a lazy list: both tables are short, and every row built is
        // every row a test or a screen reader can reach.
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 32),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              if (_notice != null)
                Card(
                  color: theme.colorScheme.primaryContainer,
                  child: Padding(
                    padding: const EdgeInsets.all(12),
                    child: Text(_notice!, key: const Key('share-notice')),
                  ),
                ),
              if (_error != null)
                Card(
                  color: theme.colorScheme.errorContainer,
                  child: Padding(
                    padding: const EdgeInsets.all(12),
                    child: Text(
                      _error!,
                      key: const Key('invite-error'),
                      style: TextStyle(
                        color: theme.colorScheme.onErrorContainer,
                      ),
                    ),
                  ),
                ),
              if (people == null)
                const Text('Loading…')
              else ...[
                _heading(
                  'People (${people.people.length})',
                  key: const Key('people-heading'),
                ),
                _table(
                  key: const Key('people-table'),
                  columns: const ['Name', 'Role', ''],
                  rows: [
                    for (final person in people.people)
                      _personRow(person, canManage),
                    for (final name in {...people.adding, ...lettingIn})
                      _addingRow(name),
                  ],
                ),
                const SizedBox(height: 4),
                Text(
                  'Names are chosen by each person.',
                  key: const Key('people-footer'),
                  style: theme.textTheme.bodySmall,
                ),
                if (!canManage) ...[
                  const SizedBox(height: 12),
                  const Text('Only the owner can add or remove people.'),
                ],
              ],
              if (canManage) ...[
                const SizedBox(height: 24),
                _heading(
                  'Invite links (${_open.length})',
                  key: const Key('invites-heading'),
                  trailing: FilledButton.icon(
                    key: const Key('new-invite'),
                    onPressed: _busy ? null : _newInvite,
                    icon: const Icon(Icons.add_link),
                    label: const Text('New invite link'),
                  ),
                ),
                const SizedBox(height: 4),
                Text(
                  'Each link lets in one person: whoever opens it first joins '
                  'the note, and moves up to People. Make one link per person '
                  'you invite. A link stops after seven days, and does not '
                  'contain the note, only a way in.',
                  style: theme.textTheme.bodySmall,
                ),
                if (widget.relayUrl case final url?)
                  if (widget.relayAccess case final access?)
                    _relayLinkSection(theme, relayLinkFor(url, access)),
                const SizedBox(height: 8),
                if (_open.isEmpty)
                  const Padding(
                    padding: EdgeInsets.symmetric(vertical: 12),
                    child: Text('No open links.', key: Key('no-invites')),
                  )
                else
                  _table(
                    key: const Key('invites-table'),
                    columns: const ['Link', 'Status', ''],
                    rows: [
                      for (final (index, invite) in _open.indexed)
                        _inviteRow(index, invite),
                    ],
                  ),
                if (people?.shared ?? false) ...[
                  const SizedBox(height: 32),
                  Align(
                    alignment: Alignment.centerLeft,
                    child: OutlinedButton.icon(
                      key: const Key('stop-sharing'),
                      style: OutlinedButton.styleFrom(
                        foregroundColor: theme.colorScheme.error,
                      ),
                      onPressed: _busy ? null : _stopSharing,
                      icon: const Icon(Icons.person_off),
                      label: const Text('Stop sharing'),
                    ),
                  ),
                ],
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// Joins by an invite link, and pops with the request it made. A link that
/// was tapped is only confirmed; the drawer's way in asks for one to be
/// pasted. The name is the account's: the button says it, and there is no
/// field for it.
class JoinDialog extends StatefulWidget {
  const JoinDialog({
    super.key,
    required this.invites,
    this.initialLink,
    this.profile,
    this.onHandOff,
  });

  final Invites invites;
  final String? initialLink;

  /// The relay the join goes through, which a link may name another of.
  final RelayProfile? profile;

  /// Called instead of joining when the link belongs to a relay this device is
  /// not on. The app moves the device there and brings the link back; the
  /// dialog closes itself first, because the library under it is replaced.
  final void Function(String link)? onHandOff;

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
    final text = _link.text.trim();
    if (text.isEmpty) return;
    // A tapped link is already in the core's form, but a pasted one can be the
    // relay page's address, its copy fallback (percent-encoded), or the whole
    // message. Normalise it once here, so the core reads one shape and a
    // hand-off carries the same link back.
    final link = inviteLinkFrom(text) ?? text;
    // A link can name the relay it belongs to. One that names a relay this
    // device is not on cannot be joined from here: the request would be posted
    // to the wrong server. The app moves the device over and brings the link
    // back, so this is one step, not a trip to settings.
    // A tapped link has been through the app already, which settled its relay
    // before showing this dialog. Handing it off again would only bring the
    // same dialog back, and a disagreement about the relay would loop.
    final named = relayFromInvite(link);
    if (named != null &&
        widget.initialLink == null &&
        widget.onHandOff != null &&
        !_isCurrentRelay(named.url)) {
      debugPrint(
        'join: handing off ${named.url} (this device: ${widget.profile?.url})',
      );
      Navigator.of(context).pop();
      widget.onHandOff!(link);
      return;
    }
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

  bool _isCurrentRelay(String url) {
    final current = widget.profile?.url;
    if (current == null) return false;
    try {
      return canonicalRelayUrl(url) == current;
    } on FormatException {
      return false;
    }
  }

  @override
  Widget build(BuildContext context) {
    final name = widget.invites.profileName ?? '';
    // A tapped link needs only a yes: showing its secret in a field to be
    // checked would ask the guest to judge something they cannot read.
    final tapped = widget.initialLink != null;
    return AlertDialog(
      title: Text(tapped ? 'Open this shared note?' : 'Join with a link'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (tapped)
            const Text(
              'The link adds you to the note. The person who sent it will see '
              'you there. It works once, for one person.',
              key: Key('join-explainer'),
            )
          else
            TextField(
              key: const Key('join-link-field'),
              controller: _link,
              autofocus: true,
              decoration: const InputDecoration(
                labelText: 'The link you were sent',
                hintText: 'Paste it here',
              ),
              onSubmitted: (_) => _ask(),
            ),
          if (widget.profile case final profile?)
            Padding(
              padding: const EdgeInsets.only(top: 12),
              child: Text(
                profile.url == null
                    ? 'This device is not set up for shared notes yet. If the '
                          'link says where the note lives, joining sets this '
                          'app up for you.'
                    : 'Joining adds the note to your EasyNotes. If the note '
                          'is kept somewhere else, this app asks before '
                          'switching.',
                key: const Key('join-relay'),
                style: Theme.of(context).textTheme.bodySmall,
              ),
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
    if (_checking || _ended != null || !appIsForeground.value) return;
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
              ? 'The person who sent the link could not open it. Ask them for '
                    'a new link.'
              : 'This link has expired or was already used. Ask for a new one.',
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
              'The person who sent the link needs EasyNotes open for a '
              'moment. You can keep waiting here.',
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
    this.handledElsewhere,
  });

  final Invites invites;

  /// A note's title, for "Anna joined Groceries".
  final String Function(String noteId) titleOf;

  /// Open the collaborators screen for a note.
  final void Function(String noteId) onReview;

  /// A request of this device's was let in and its note has opened.
  final void Function(String noteId) onJoined;

  /// Whether something else — a wait dialog for that join — is answering it,
  /// and will open the note itself. Asked on every check, so an answer is never
  /// taken from under the dialog that is showing it.
  final bool Function(String inviteId)? handledElsewhere;

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
    if (_checking || !appIsForeground.value) return;
    _checking = true;
    try {
      final joined = <String, _Joined>{};
      for (final invite in widget.invites.allInvites) {
        try {
          final status = await widget.invites.checkInvite(invite.id);
          if (status.joined != null) {
            joined[invite.id] = _Joined(invite.noteId, status.joined!);
            // Once: the invite is forgotten as it reports who joined. The
            // collaborators screen, if it caught the join first, says so
            // itself.
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
        // A wait dialog is showing this one; it opens the note when it lands.
        if (widget.handledElsewhere?.call(join.inviteId) ?? false) continue;
        try {
          final check = await widget.invites.checkJoin(join.inviteId);
          switch (check.state) {
            case JoinState.joined:
              // A note the link's pre-grant already opened has nothing to
              // announce: it is on screen, and it was never a wait.
              if (check.noteId != null && !join.readable) {
                widget.onJoined(check.noteId!);
              }
            case JoinState.declined:
              _ended[join.inviteId] =
                  'A link you opened could not be used. Ask for a new one.';
            case JoinState.gone:
              _ended[join.inviteId] =
                  'This link has expired or was already used. Ask for a new '
                  'one.';
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
        if (!join.readable)
          _Line(
            key: Key('join-banner-${join.inviteId}'),
            colour: scheme.secondaryContainer,
            text: 'Joining a shared note…',
            action: 'Stop waiting',
            onAction: () async {
              await widget.invites.cancelJoin(join.inviteId);
              if (mounted) {
                setState(() => _waiting = widget.invites.pendingJoins);
              }
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
