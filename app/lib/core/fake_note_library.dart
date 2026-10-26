/// An in-memory [NoteLibrary] for widget tests.
///
/// The same bargain as [FakeNoteCore]: it is not a CRDT and not a store, and it
/// does not pretend to be. What it has to get right is the contract the note
/// list screen depends on — a note created or joined appears in the list, an
/// edit to a note changes its card, and the list arrives on a stream rather than
/// being handed over to be kept.
library;

import 'dart:async';
import 'dart:typed_data';

import 'attachments.dart';
import 'fake_note_core.dart';
import 'note_core.dart';
import 'note_library.dart';
import 'sync_status.dart';

/// A [StorageHealth] on its own, for a screen that wants one without a library.
///
/// The checklist screen is the case: it is given one note and the device's
/// answer to "is this being kept?", and nothing else.
class FakeStorageHealth implements StorageHealth {
  final _errors = StreamController<String?>.broadcast();
  String? _error;

  @override
  String? get storageError => _error;

  @override
  Stream<String?> watchStorageError() async* {
    yield _error;
    yield* _errors.stream;
  }

  /// Say that a write was refused, without a browser to refuse it.
  void push(String error) {
    _error = error;
    _errors.add(error);
  }

  Future<void> dispose() => _errors.close();
}

class FakeNoteLibrary implements NoteLibrary {
  FakeNoteLibrary({Map<String, FakeNoteCore>? notes, this.canJoin = true}) {
    if (notes != null) {
      // In the order they are written: a test that writes `{'a': …, 'b': …}`
      // means "a was made first", and the real library lists newest first.
      for (final entry in notes.entries) {
        _add(entry.key, entry.value);
      }
    }
    _emit();
  }

  final _notes = StreamController<List<NoteSummary>>.broadcast();
  final _sync = StreamController<SyncStatus>.broadcast();
  final _lastSynced = StreamController<DateTime?>.broadcast();
  final _storageErrors = StreamController<String?>.broadcast();
  String? _storageError;

  /// Newest first, as the real one is.
  final List<String> _order = [];
  final Map<String, FakeNoteCore> _cores = {};
  final Map<String, StreamSubscription<void>> _watchers = {};
  final Map<String, _Placement> _placements = {};
  final Map<String, Reminder> _reminders = {};

  /// Every label this fake device has, by ID — the `l:<id>` half of the account
  /// document. A label lives here whether or not a note carries it, which is
  /// the thing an ID buys and the reason `tags` can report a count of zero.
  final Map<String, String> _labels = {};
  int _nextLabel = 1;
  NoteLayout _layout = NoteLayout.grid;
  bool _tickedShown = false;
  int _counter = 0;
  int _nextReminder = 1;

  /// Every call the screen made, in order. Tests assert on this to show that a
  /// gesture reached the library rather than only changing pixels.
  final List<String> calls = [];

  /// Holds [openNote] at the door when set, so a test can act while the note is
  /// still opening.
  Completer<void>? openGate;

  List<NoteSummary> _summaries = const [];
  SyncStatus _status = const SyncStatus.offline();
  DateTime? _lastSyncedAt;

  @override
  final bool canJoin;

  /// The core behind one note, for a test that wants to edit it from underneath
  /// the screen.
  FakeNoteCore core(String id) => _cores[id]!;

  @override
  List<NoteSummary> get notes => _summaries;

  @override
  Stream<List<NoteSummary>> watchNotes() async* {
    yield _summaries;
    yield* _notes.stream;
  }

  @override
  List<Reminder> get reminders {
    final all = _reminders.values.toList()
      ..sort((a, b) {
        final due = a.fireAt.compareTo(b.fireAt);
        return due != 0 ? due : a.id.compareTo(b.id);
      });
    return all;
  }

  @override
  Stream<List<Reminder>> watchReminders() => watchNotes().map((_) => reminders);

  @override
  Future<String> createReminder({
    required String noteId,
    required DateTime fireAt,
    required ReminderRepeat repeat,
    required String timezone,
  }) async {
    final id = 'reminder-${_nextReminder++}';
    calls.add('createReminder($noteId, $id)');
    _reminders[id] = Reminder(
      id: id,
      noteId: noteId,
      fireAt: fireAt,
      repeat: repeat,
      timezone: timezone,
    );
    _emit();
    return id;
  }

  @override
  Future<void> setReminder(Reminder reminder) async {
    calls.add('setReminder(${reminder.id})');
    _reminders[reminder.id] = reminder;
    _emit();
  }

  @override
  Future<void> forgetReminder(String reminderId) async {
    calls.add('forgetReminder($reminderId)');
    _reminders.remove(reminderId);
    _emit();
  }

  @override
  Future<String> createNote({NoteKind kind = NoteKind.checklist}) async {
    calls.add(
      kind == NoteKind.checklist ? 'createNote()' : 'createNote(${kind.name})',
    );
    final id = 'note-${_counter++}';
    _add(id, FakeNoteCore(kind: kind));
    _emit();
    return id;
  }

  // -- invites --------------------------------------------------------------
  //
  // A relay in a map. The two `fake…` methods at the end are the other person:
  // a guest opening this device's link, and an owner's device answering this
  // device's request.

  final Map<String, Invite> _invites = {};

  /// Who opened each invite's link, once somebody has.
  final Map<String, String> _opened = {};
  final Map<String, PendingJoin> _joins = {};
  final Map<String, JoinCheck> _answers = {};

  /// What [createInvite] and [requestJoin] throw, to stand in for a device
  /// with no relay.
  String? relayError;

  /// The note a link hands over when it is opened, as the pre-grant does
  /// (ADR 0030). Null is a link that has to wait for the owner's device.
  String? grantOnJoin;

  final Map<String, People> _people = {};

  /// Whether a removal takes effect at once, as it does online, or waits and
  /// shows as “Removing…”, as it does offline.
  bool online = true;

  final List<Removal> _removals = [];

  Person get _me => Person(
    member: 'me',
    invite: '',
    name: profileName ?? '',
    owner: true,
    me: true,
  );

  @override
  People people(String noteId) =>
      _people[noteId] ?? People(canManage: true, people: [_me]);

  /// Stand a note's list up as a test needs it.
  void setPeople(String noteId, People value) {
    _people[noteId] = value;
    _emit();
  }

  void _setList(String noteId, List<Person> list, {List<String>? adding}) {
    final before = people(noteId);
    setPeople(
      noteId,
      People(
        shared: list.any((p) => !p.owner),
        canManage: before.canManage,
        people: list,
        adding: adding ?? before.adding,
      ),
    );
  }

  void _requireSharing() {
    if (!hasAccount) throw 'this device has no account yet';
    if (profileName == null) throw 'sharing needs a name: set one up first';
  }

  @override
  Future<Invite> createInvite(String noteId) async {
    calls.add('createInvite($noteId)');
    _requireSharing();
    if (relayError != null) throw relayError!;
    final id = 'invite-${_counter++}';
    final invite = Invite(
      id: id,
      noteId: noteId,
      link: 'easynotes://join#$id',
      expiresIn: const Duration(days: 7),
    );
    _invites[id] = invite;
    return invite;
  }

  @override
  List<Invite> invitesFor(String noteId) => [
    for (final invite in _invites.values)
      if (invite.noteId == noteId) invite,
  ];

  @override
  List<Invite> get allInvites => _invites.values.toList();

  @override
  Future<InviteStatus> checkInvite(String inviteId) async {
    calls.add('checkInvite($inviteId)');
    final invite = _invites[inviteId];
    if (invite == null) throw 'This device has no such invite open.';
    final name = _opened[inviteId];
    if (name == null) {
      return const InviteStatus(open: true, expiresIn: Duration(days: 7));
    }
    if (!online) {
      return InviteStatus(
        open: false,
        expiresIn: const Duration(days: 7),
        lettingIn: name,
      );
    }
    _invites.remove(inviteId);
    _opened.remove(inviteId);
    final noteId = invite.noteId;
    _setList(noteId, [
      ...people(noteId).people,
      Person(member: 'member-$inviteId', invite: inviteId, name: name),
    ]);
    return InviteStatus(
      open: false,
      expiresIn: const Duration(days: 7),
      joined: name,
    );
  }

  @override
  Future<void> withdrawInvite(String inviteId) async {
    calls.add('withdrawInvite($inviteId)');
    _invites.remove(inviteId);
    _opened.remove(inviteId);
  }

  @override
  Future<void> removePerson(String noteId, Person person) async {
    calls.add('removePerson($noteId, ${person.member})');
    final list = people(noteId).people;
    _setList(noteId, [
      for (final p in list)
        if (p.member != person.member)
          p
        else if (!online)
          Person(
            member: p.member,
            invite: p.invite,
            name: p.name,
            owner: p.owner,
            me: p.me,
            removing: true,
          ),
    ]);
  }

  /// This device reached the relay: every removal waiting is made.
  void finishRemovals() {
    for (final noteId in _people.keys.toList()) {
      _setList(noteId, [
        for (final p in people(noteId).people)
          if (!p.removing) p,
      ]);
    }
  }

  @override
  Future<void> stopSharing(String noteId) async {
    calls.add('stopSharing($noteId)');
    for (final invite in invitesFor(noteId)) {
      await withdrawInvite(invite.id);
    }
    for (final person in people(noteId).people.where((p) => !p.owner)) {
      await removePerson(noteId, person);
    }
  }

  @override
  List<Removal> takeRemovals() {
    final taken = List.of(_removals);
    _removals.clear();
    return taken;
  }

  /// The owner removed this person: the note leaves this device, with one
  /// message.
  void fakeRemoved(String noteId) {
    final title = _cores[noteId]?.current.title ?? '';
    _forget(noteId);
    _people.remove(noteId);
    _removals.add(Removal(noteId: noteId, title: title));
    _emit();
  }

  @override
  Future<PendingJoin> requestJoin(String link) async {
    calls.add('requestJoin($link)');
    _requireSharing();
    if (relayError != null) throw relayError!;
    if (!link.contains('#')) throw 'that is not an invite link: not base64url';
    final id = link.split('#').last;
    final join = PendingJoin(
      inviteId: id,
      name: profileName!,
      noteId: grantOnJoin,
    );
    _joins[id] = join;
    return join;
  }

  @override
  List<PendingJoin> get pendingJoins => _joins.values.toList();

  @override
  Future<JoinCheck> checkJoin(String inviteId) async {
    calls.add('checkJoin($inviteId)');
    final answer = _answers.remove(inviteId);
    if (answer == null) return const JoinCheck(JoinState.waiting);
    _joins.remove(inviteId);
    return answer;
  }

  @override
  Future<void> cancelJoin(String inviteId) async {
    calls.add('cancelJoin($inviteId)');
    _joins.remove(inviteId);
  }

  /// Somebody opened this device's link. The next [checkInvite] lets them
  /// in, as the real one does.
  void fakeOpened(String inviteId, {required String name}) {
    _opened[inviteId] = name;
  }

  /// The owner's device answered the request this device is waiting on.
  /// Letting it in adds a note, as the real one does.
  void fakeAnswer(String inviteId, JoinState state, {String? message}) {
    if (state == JoinState.joined) {
      final id = 'joined-${_counter++}';
      _add(id, FakeNoteCore(title: 'Shared list'));
      _emit();
      _answers[inviteId] = JoinCheck(state, noteId: id);
    } else {
      _answers[inviteId] = JoinCheck(state, message: message);
    }
  }

  /// The same rule the core follows: every whitespace-separated word has to
  /// appear somewhere in the note, case ignored, and the line that comes back is
  /// the first one — title, then the body's lines, then items — holding any
  /// of them.
  @override
  Future<List<SearchHit>> search(String query) async {
    calls.add('search($query)');
    final terms = query.toLowerCase().split(RegExp(r'\s+'))
      ..removeWhere((term) => term.isEmpty);
    if (terms.isEmpty) return const [];
    final hits = <SearchHit>[];
    for (final note in _summaries) {
      final view = _cores[note.id]!.current;
      final lines = [
        view.title,
        ..._bodyLines(view.body),
        for (final item in view.items) item.text,
      ];
      bool holds(String term) =>
          lines.any((line) => line.toLowerCase().contains(term));
      if (!terms.every(holds)) continue;
      final matched = lines.firstWhere(
        (line) => terms.any((term) => line.toLowerCase().contains(term)),
      );
      hits.add(SearchHit(note: note, matched: matched));
    }
    return hits;
  }

  @override
  Future<NoteCore> openNote(String noteId) async {
    calls.add('openNote($noteId)');
    // A test can hold a note at the door to see what the screen does while it
    // is still opening — a real core has to go and fetch it.
    await openGate?.future;
    return _cores[noteId] ?? (throw StateError('no note $noteId'));
  }

  @override
  Future<void> setColour(String noteId, NoteColour? colour) async {
    calls.add('setColour($noteId, ${colour?.name})');
    _placement(noteId).colour = colour;
    _emit();
  }

  @override
  Future<void> setPinned(String noteId, bool pinned) async {
    calls.add('setPinned($noteId, $pinned)');
    _placement(noteId).pinned = pinned;
    _emit();
  }

  /// The same rule the core follows, over the list this holds instead of over
  /// fractional indices: dragged downwards a note lands after the card it was
  /// dropped on, dragged upwards it lands before it.
  @override
  Future<void> moveNoteTo(String noteId, String targetId) async {
    calls.add('moveNoteTo($noteId, $targetId)');
    if (noteId == targetId) return;
    final from = _order.indexOf(noteId);
    final to = _order.indexOf(targetId);
    if (from < 0 || to < 0) return;
    _order.removeAt(from);
    final at = _order.indexOf(targetId);
    _order.insert(from < to ? at + 1 : at, noteId);
    _emit();
  }

  /// Counted off the notes, as the real one does: a label exists because a note
  /// carries it, and sorted so a menu holds still.
  @override
  List<Tag> get tags {
    final counts = {for (final id in _labels.keys) id: 0};
    for (final placement in _placements.values) {
      for (final id in placement.tags) {
        counts[id] = (counts[id] ?? 0) + 1;
      }
    }
    final ids = counts.keys.toList()
      ..sort((a, b) {
        final left = _labels[a]!, right = _labels[b]!;
        final folded = left.toLowerCase().compareTo(right.toLowerCase());
        if (folded != 0) return folded;
        final exact = left.compareTo(right);
        return exact != 0 ? exact : a.compareTo(b);
      });
    return [
      for (final id in ids) Tag(id: id, name: _labels[id]!, notes: counts[id]!),
    ];
  }

  /// The core's rules, kept here so the screen sees the same thing from both.
  String? _normalise(String raw) {
    final name = raw.trim().replaceAll(RegExp(r'\s+'), ' ');
    return name.isEmpty ? null : name;
  }

  /// A label already here whose name matches ignoring case, or null.
  String? _labelNamed(String name) {
    final matches =
        _labels.entries
            .where((e) => e.value.toLowerCase() == name.toLowerCase())
            .map((e) => e.key)
            .toList()
          ..sort();
    return matches.isEmpty ? null : matches.first;
  }

  @override
  Future<String?> addTag(String noteId, String name) async {
    calls.add('addTag($noteId, $name)');
    final wanted = _normalise(name);
    if (wanted == null) return null;
    final id = _labelNamed(wanted) ?? _mint(wanted);
    _placement(noteId).tags.add(id);
    _emit();
    return id;
  }

  @override
  Future<String> makeTag(String name) async {
    calls.add('makeTag($name)');
    final wanted = _normalise(name);
    if (wanted == null) return '';
    final id = _labelNamed(wanted) ?? _mint(wanted);
    _emit();
    return id;
  }

  String _mint(String name) {
    final id = 'label-${_nextLabel++}';
    _labels[id] = name;
    return id;
  }

  @override
  Future<void> removeTag(String noteId, String labelId) async {
    calls.add('removeTag($noteId, $labelId)');
    _placement(noteId).tags.remove(labelId);
    _emit();
  }

  /// Renaming onto a name another label already has merges the two, as the core
  /// does — every note on the renamed label gains the survivor.
  @override
  Future<String?> renameTag(String labelId, String name) async {
    calls.add('renameTag($labelId, $name)');
    final wanted = _normalise(name);
    if (wanted == null || !_labels.containsKey(labelId)) return null;
    final existing = _labelNamed(wanted);
    if (existing != null && existing != labelId) {
      for (final placement in _placements.values) {
        if (placement.tags.remove(labelId)) placement.tags.add(existing);
      }
      _labels.remove(labelId);
      _emit();
      return existing;
    }
    _labels[labelId] = wanted;
    _emit();
    return labelId;
  }

  @override
  Future<void> forgetTag(String labelId) async {
    calls.add('forgetTag($labelId)');
    _labels.remove(labelId);
    for (final placement in _placements.values) {
      placement.tags.remove(labelId);
    }
    _emit();
  }

  @override
  Future<void> setArchived(String noteId, bool archived) async {
    calls.add('setArchived($noteId, $archived)');
    final placement = _placement(noteId);
    placement.archived = archived;
    // The core's rule, kept here so the screen sees the same thing from both: a
    // note cannot be both held at the top and put out of the way.
    if (archived) placement.pinned = false;
    _emit();
  }

  @override
  Future<void> setTrashed(String noteId, bool trashed) async {
    calls.add('setTrashed($noteId, $trashed)');
    final placement = _placement(noteId);
    placement.trashed = trashed;
    if (trashed) placement.pinned = false;
    _emit();
  }

  /// When set, [forgetNote] and [emptyTrash] throw it, having done nothing,
  /// as a store that refused the deletion's first write would.
  Object? refuseDeletion;

  @override
  Future<void> forgetNote(String noteId) async {
    calls.add('forgetNote($noteId)');
    if (refuseDeletion case final error?) throw error;
    _reminders.removeWhere((_, reminder) => reminder.noteId == noteId);
    _forget(noteId);
    _emit();
  }

  @override
  Future<void> setColourMany(List<String> noteIds, NoteColour? colour) async {
    calls.add('setColourMany(${noteIds.join(', ')}; ${colour?.name})');
    for (final id in noteIds) {
      _placement(id).colour = colour;
    }
    _emit();
  }

  @override
  Future<void> setPinnedMany(List<String> noteIds, bool pinned) async {
    calls.add('setPinnedMany(${noteIds.join(', ')}; $pinned)');
    for (final id in noteIds) {
      _placement(id).pinned = pinned;
    }
    _emit();
  }

  @override
  Future<void> setArchivedMany(List<String> noteIds, bool archived) async {
    calls.add('setArchivedMany(${noteIds.join(', ')}; $archived)');
    for (final id in noteIds) {
      final placement = _placement(id);
      placement.archived = archived;
      if (archived) placement.pinned = false;
    }
    _emit();
  }

  @override
  Future<void> setTrashedMany(List<String> noteIds, bool trashed) async {
    calls.add('setTrashedMany(${noteIds.join(', ')}; $trashed)');
    for (final id in noteIds) {
      final placement = _placement(id);
      placement.trashed = trashed;
      if (trashed) placement.pinned = false;
    }
    _emit();
  }

  @override
  Future<String?> addTagMany(List<String> noteIds, String name) async {
    calls.add('addTagMany(${noteIds.join(', ')}; $name)');
    final wanted = _normalise(name);
    if (wanted == null) return null;
    final label = _labelNamed(wanted) ?? _mint(wanted);
    for (final id in noteIds) {
      _placement(id).tags.add(label);
    }
    _emit();
    return label;
  }

  @override
  Future<void> removeTagMany(List<String> noteIds, String labelId) async {
    calls.add('removeTagMany(${noteIds.join(', ')}; $labelId)');
    for (final id in noteIds) {
      _placement(id).tags.remove(labelId);
    }
    _emit();
  }

  @override
  Future<int> forgetNotes(List<String> noteIds) async {
    calls.add('forgetNotes(${noteIds.join(', ')})');
    if (refuseDeletion case final error?) throw error;
    final ids = noteIds.toSet();
    for (final id in ids) {
      _reminders.removeWhere((_, reminder) => reminder.noteId == id);
      _forget(id);
    }
    _emit();
    return ids.length;
  }

  @override
  Future<int> emptyTrash() async {
    calls.add('emptyTrash()');
    if (refuseDeletion case final error?) throw error;
    final trashed = [
      for (final id in _order)
        if (_placements[id]?.trashed ?? false) id,
    ];
    for (final id in trashed) {
      _forget(id);
    }
    _emit();
    return trashed.length;
  }

  @override
  NoteLayout get layout => _layout;

  @override
  Future<void> setLayout(NoteLayout layout) async {
    calls.add('setLayout(${layout.name})');
    _layout = layout;
  }

  @override
  bool get tickedShown => _tickedShown;

  @override
  Future<void> setTickedShown(bool shown) async {
    calls.add('setTickedShown($shown)');
    _tickedShown = shown;
  }

  /// The twelve words this fake hands out, so a test can assert on what a
  /// screen showed without minting real entropy for it.
  static const phrase = [
    'abandon',
    'ability',
    'able',
    'about',
    'above',
    'absent',
    'absorb',
    'abstract',
    'absurd',
    'abuse',
    'access',
    'accident',
  ];

  bool _hasAccount = false;
  String? _profileName;

  @override
  bool get hasAccount => _hasAccount;

  @override
  String? get profileName => _profileName;

  @override
  Future<void> setProfileName(String name) async {
    calls.add('setProfileName($name)');
    if (!_hasAccount) throw 'this device has no account yet';
    if (name.trim().isEmpty) throw 'sharing needs a name: set one up first';
    _profileName = name.trim();
    // The name is drawn in the list and on every share screen, and the real
    // core writes it into each shared note; the fake's `_me` reads it.
    _emit();
  }

  /// A device that has already set sharing up, as most tests want.
  void setUpSharing([String name = 'Olga']) {
    _hasAccount = true;
    _profileName = name;
  }

  @override
  Future<List<String>> createAccount() async {
    calls.add('createAccount()');
    if (_hasAccount) {
      throw StateError('this device already has an account');
    }
    _hasAccount = true;
    return phrase;
  }

  /// What [deleteAccount] throws, when a test wants the relay to refuse.
  Object? deleteAccountFails;

  @override
  Future<void> deleteAccount() async {
    calls.add('deleteAccount()');
    if (deleteAccountFails case final error?) throw error;
    for (final id in _cores.keys.toList()) {
      await forgetNote(id);
    }
    _hasAccount = false;
    _profileName = null;
    _emit();
  }

  @override
  Future<void> adoptAccount(String words) async {
    calls.add('adoptAccount($words)');
    if (_hasAccount) {
      throw StateError('this device already has an account');
    }
    // Twelve of them or it is not a phrase. The real one also checks that they
    // are words and that the checksum holds; a fake that refused everything
    // real would make the screen untestable, and one that accepted anything
    // would let a screen skip the error path altogether.
    if (words.split(RegExp(r'\s+')).where((w) => w.isNotEmpty).length != 12) {
      throw ArgumentError('a recovery phrase is twelve words');
    }
    _hasAccount = true;
    // What the account's other devices had, arriving as the real one's do
    // once it reaches the relay: shared notes included, with nobody asked.
    for (final MapEntry(key: id, value: core) in restoredWithAccount.entries) {
      _add(id, core);
    }
    restoredWithAccount.clear();
    _emit();
  }

  /// Notes this person has on their other devices, which appear once this
  /// device adopts their account.
  final Map<String, FakeNoteCore> restoredWithAccount = {};

  @override
  Future<void> connect(String url, {String? access}) async {
    if (!hasAccount) {
      throw StateError('Sync needs an account. Set up sync first.');
    }
    calls.add(access == null ? 'connect($url)' : 'connect($url, $access)');
    pushStatus(const SyncStatus(state: SyncState.connecting));
  }

  /// What [verifyRelay] throws, when a test wants an address that does not
  /// answer. Null for a relay that does.
  Object? verifyRelayFails;

  @override
  Future<void> verifyRelay(String url, {String? access}) async {
    calls.add(
      access == null ? 'verifyRelay($url)' : 'verifyRelay($url, $access)',
    );
    if (verifyRelayFails case final error?) throw error;
  }

  @override
  Future<void> disconnect() async {
    calls.add('disconnect()');
    pushStatus(const SyncStatus.offline());
  }

  @override
  void sendHeldEdits() => calls.add('sendHeldEdits()');

  @override
  Future<void> setForeground(bool foreground) async =>
      calls.add('setForeground($foreground)');

  @override
  Future<void> syncNow() async => calls.add('syncNow()');

  List<AttachmentTransfer> _transferList = const [];
  final _transferStream =
      StreamController<List<AttachmentTransfer>>.broadcast();

  @override
  Stream<List<AttachmentTransfer>> watchTransfers() async* {
    yield _transferList;
    yield* _transferStream.stream;
  }

  /// Say what the transfer queue would say, without a relay to transfer to.
  void pushTransfers(List<AttachmentTransfer> transfers) {
    _transferList = transfers;
    _transferStream.add(transfers);
  }

  @override
  void retryTransfers() => calls.add('retryTransfers()');

  AttachmentView? _cover(String id) => _cores[id]!.current.attachments
      .where((a) => a.kind == AttachmentKind.image && a.hasThumbnail)
      .firstOrNull;

  @override
  Future<Uint8List?> readCover(String noteId) async {
    final cover = _cover(noteId);
    if (cover == null) return null;
    return _cores[noteId]!.readAttachment(cover.id, thumbnail: true);
  }

  @override
  Future<void> flush() async => calls.add('flush()');

  @override
  SyncStatus get syncStatus => _status;

  @override
  Stream<SyncStatus> watchSync() async* {
    yield _status;
    yield* _sync.stream;
  }

  /// Say what the engines would say, without a network to say it with.
  void pushStatus(SyncStatus status) {
    _status = status;
    _sync.add(status);
    // The same rule the real library applies: a caught-up status is when a
    // "last synced" footer can point at.
    if (status.state == SyncState.synced && status.pending == 0) {
      _lastSyncedAt = DateTime.now();
      _lastSynced.add(_lastSyncedAt);
    }
  }

  @override
  DateTime? get lastSyncedAt => _lastSyncedAt;

  @override
  Stream<DateTime?> watchLastSyncedAt() async* {
    yield _lastSyncedAt;
    yield* _lastSynced.stream;
  }

  @override
  String? get storageError => _storageError;

  @override
  Stream<String?> watchStorageError() async* {
    yield _storageError;
    yield* _storageErrors.stream;
  }

  /// Say that a write was refused, without a browser to refuse it.
  ///
  /// It says exactly what it is told and latches nothing. Latching is the real
  /// library's decision and `rust_bridge_test.dart` is where it is held; a fake
  /// that made it too would be a second copy of a rule, and a screen test that
  /// passed because of it would be testing this file.
  void pushStorageError(String error) {
    _storageError = error;
    _storageErrors.add(error);
  }

  @override
  Future<void> dispose() async {
    for (final watcher in _watchers.values) {
      await watcher.cancel();
    }
    await _notes.close();
    await _sync.close();
    await _lastSynced.close();
    await _storageErrors.close();
    await _transferStream.close();
  }

  void _add(String id, FakeNoteCore core) {
    _order.insert(0, id);
    _cores[id] = core;
    // The real list is refreshed by the core on every edit, wherever it came
    // from. Here that means watching each note, so that a test editing a core
    // directly — standing in for a peer's edit arriving — moves the card.
    _watchers[id] = core.watch().listen((_) => _emit());
  }

  _Placement _placement(String id) =>
      _placements.putIfAbsent(id, _Placement.new);

  /// Everything the real library forgets: the note, this person's placement of
  /// it, and the watcher behind its card. Not `_emit`: the callers above do that
  /// once, so emptying a trash of ten notes is one list and not ten.
  void _forget(String id) {
    _order.remove(id);
    _cores.remove(id);
    _placements.remove(id);
    _watchers.remove(id)?.cancel();
  }

  /// A body as the real index keeps it: its lines, minus the blank ones.
  static List<String> _bodyLines(String body) => [
    for (final line in body.split('\n'))
      if (line.trim().isNotEmpty) line,
  ];

  void _emit() {
    final summaries = [
      for (final id in _order)
        NoteSummary(
          collaborators: [
            for (final p in people(id).people)
              if (!p.me) Collaborator(id: p.member, name: p.name),
          ],
          id: id,
          kind: _cores[id]!.current.kind,
          title: _cores[id]!.current.title,
          items: _cores[id]!.current.items.length,
          checked: _cores[id]!.current.checkedCount,
          preview: [
            ..._bodyLines(_cores[id]!.current.body),
            for (final item in _cores[id]!.current.items)
              if (!item.checked) item.text,
          ].take(4).toList(),
          images: _cores[id]!.current.attachments
              .where((a) => a.kind == AttachmentKind.image)
              .length,
          audio: _cores[id]!.current.attachments
              .where((a) => a.kind == AttachmentKind.audio)
              .length,
          cover: _cover(id)?.id,
          coverWidth: _cover(id)?.thumbnailWidth,
          coverHeight: _cover(id)?.thumbnailHeight,
          colour: _placements[id]?.colour,
          pinned: _placements[id]?.pinned ?? false,
          archived: _placements[id]?.archived ?? false,
          trashed: _placements[id]?.trashed ?? false,
          tags: _labelsOn(id),
        ),
    ];
    // Pinned first, as the core sorts them; the rest keep the order they were
    // made in. A stable sort, so pinning one note does not reshuffle the others.
    final pinned = summaries.where((s) => s.pinned);
    final rest = summaries.where((s) => !s.pinned);
    _summaries = [...pinned, ...rest];
    _notes.add(_summaries);
  }
}

/// The labels on a note, resolved to names and sorted as a menu sorts them —
/// which is what the core puts on a summary.
extension on FakeNoteLibrary {
  List<Label> _labelsOn(String id) {
    final on = _placements[id]?.tags ?? const <String>{};
    final labels =
        [
          for (final labelId in on)
            if (_labels[labelId] != null)
              Label(id: labelId, name: _labels[labelId]!),
        ]..sort((a, b) {
          final folded = a.name.toLowerCase().compareTo(b.name.toLowerCase());
          if (folded != 0) return folded;
          final exact = a.name.compareTo(b.name);
          return exact != 0 ? exact : a.id.compareTo(b.id);
        });
    return labels;
  }
}

/// What the fake remembers about a note, standing in for the account document.
class _Placement {
  NoteColour? colour;
  bool pinned = false;
  bool archived = false;
  bool trashed = false;

  /// Label IDs, not names — the same separation the account document makes.
  final Set<String> tags = {};
}

/// Notifications that write down what they would have said.
class RecordingJoinAlerts implements JoinAlerts {
  final List<String> calls = [];

  @override
  Future<void> requestPermission() async => calls.add('requestPermission()');

  @override
  Future<void> joined({
    required String inviteId,
    required String noteId,
    required String name,
  }) async => calls.add('joined($inviteId, $noteId, $name)');
}
