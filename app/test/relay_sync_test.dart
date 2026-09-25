/// Demo steps 1–3 from the app's own API, through a real relay.
///
/// `relay/tests/engine.rs` proves the sync engine in Rust, and the widget tests
/// prove the screen against a fake. This is the seam between them: two cores
/// opened the way the app opens them, connected the way the app connects them,
/// talking to a relay process over a real socket. It is what Slice 1's third gap
/// asked for — the demo run from the app's side of the bridge rather than only
/// headlessly.
///
/// Skipped unless `EASYNOTES_NATIVE_LIB` and `EASYNOTES_RELAY_BIN` are set.
/// `just test-app` and CI set both.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:easynotes/core/master_key.dart';
import 'package:easynotes/core/note_core.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/core/rust_note_core.dart';
import 'package:easynotes/core/rust_note_library.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';

/// Long enough that a loaded machine is slow rather than red.
const patience = Duration(seconds: 20);

void main() {
  final libPath = Platform.environment['EASYNOTES_NATIVE_LIB'];
  final relayPath = Platform.environment['EASYNOTES_RELAY_BIN'];
  final skip = libPath == null || relayPath == null
      ? 'set EASYNOTES_NATIVE_LIB and EASYNOTES_RELAY_BIN to run these'
      : null;

  Process? relay;
  late String relayUrl;

  setUpAll(() async {
    if (skip != null) return;
    await RustNoteCore.ensureInitialized(
      library: ExternalLibrary.open(libPath!),
    );
    // Port 0: the OS picks, the relay logs what it got, and tests can run in
    // parallel without fighting over a port.
    final started = await Process.start(
      relayPath!,
      const [],
      environment: {
        'RELAY_ADDR': '127.0.0.1:0',
        'RUST_LOG': 'easynotes_relay=info',
        'NO_COLOR': '1',
      },
    );
    relay = started;
    relayUrl = 'http://${await _listeningAddress(started)}';
  });

  tearDownAll(() {
    // Not guarded by `skip`: if `setUpAll` failed after starting the relay, this
    // is the only thing that will stop it.
    relay?.kill();
  });

  /// A device: its own directory, its own library, connected to the relay the
  /// way the app connects one.
  Future<RustNoteLibrary> device() async {
    final dir = await Directory.systemTemp.createTemp('easynotes-relay-test');
    final library = await RustNoteLibrary.open(
      dir.path,
      MasterKeyStore.newKey(),
    );
    addTearDown(() async {
      await library.dispose();
      await dir.delete(recursive: true);
    });
    await library.connect(relayUrl);
    return library;
  }

  /// A device with one new note on it, which is what most of these want.
  Future<(RustNoteLibrary, NoteCore)> open() async {
    final library = await device();
    final core = library.openNote(await library.createNote());
    addTearDown(core.dispose);
    return (library, core);
  }

  /// Keep checking the invite until whoever opened the link is in — which
  /// is what the invite dialog's poll, and the grid's, does. Nobody approves
  /// anything. Returns the name of the person let in.
  Future<String> letIn(RustNoteLibrary owner, Invite invite) async {
    final deadline = DateTime.now().add(patience);
    while (true) {
      try {
        final status = await owner.checkInvite(invite.id);
        if (status.joined != null) return status.joined!;
      } catch (_) {}
      if (DateTime.now().isAfter(deadline)) fail('nobody was let in');
      await Future<void>.delayed(const Duration(milliseconds: 50));
    }
  }

  /// Ask until the note has opened, which is a moment after being let in.
  Future<JoinCheck> joined(RustNoteLibrary guest, String inviteId) async {
    final deadline = DateTime.now().add(patience);
    while (true) {
      final check = await guest.checkJoin(inviteId);
      if (check.state != JoinState.opening &&
          check.state != JoinState.waiting) {
        return check;
      }
      if (DateTime.now().isAfter(deadline)) fail('the note never opened');
      await Future<void>.delayed(const Duration(milliseconds: 50));
    }
  }

  /// A second person on `noteId`, let in the way the app lets one in: a link
  /// they open, and the owner's device noticing. The owner is given an account and a name first
  /// if it has none, since sharing needs both.
  Future<(RustNoteLibrary, NoteCore)> share(
    RustNoteLibrary owner,
    String noteId,
  ) async {
    if (!owner.hasAccount) await owner.createAccount();
    if (owner.profileName == null) await owner.setProfileName('Olga');
    final invite = await owner.createInvite(noteId);
    final guest = await device();
    await guest.createAccount();
    await guest.setProfileName('Anna');
    final waiting = await guest.requestJoin(invite.link);
    await letIn(owner, invite);
    final check = await joined(guest, waiting.inviteId);
    expect(check.state, JoinState.joined);
    final core = guest.openNote(check.noteId!);
    addTearDown(core.dispose);
    return (guest, core);
  }

  group('two devices through a relay', () {
    test(
      'a device that joins sees the list, and edits flow both ways',
      () async {
        final (aLibrary, a) = await open();
        await a.setTitle('Groceries');
        for (final item in ['milk', 'bread', 'eggs']) {
          await a.addItem(item);
        }

        // Demo step 2: B joins by invite, having never been connected when the
        // edits were made.
        final (_, b) = await share(aLibrary, aLibrary.notes.single.id);
        await _until(
          b.watch(),
          (view) =>
              view.items.map((i) => i.text).toList().join(',') ==
              'milk,bread,eggs',
          'B never received A\'s list',
        );
        expect(b.current.title, 'Groceries');

        // Demo step 3: B checks an item and A sees it, with nobody polling.
        final bread = b.current.items.firstWhere((i) => i.text == 'bread');
        await b.setChecked(bread.id, true);
        await _until(
          a.watch(),
          (view) => view.items.any((i) => i.text == 'bread' && i.checked),
          'A never saw B check an item',
        );
      },
    );

    test(
      'a title typed a letter at a time arrives whole, and a flush sends it',
      () async {
        final (aLibrary, a) = await open();
        final (_, b) = await share(aLibrary, aLibrary.notes.single.id);

        // Typing is held back until it pauses (ADR 0016), so each of these
        // is logged at once and sent later, as one.
        const title = 'Groceries';
        for (var end = 1; end <= title.length; end++) {
          await a.setTitle(title.substring(0, end));
        }
        // What the app does going into the background: rather than wait for
        // the pause, send what is held now.
        aLibrary.sendHeldEdits();

        await _until(
          b.watch(),
          (view) => view.title == title,
          'B never received the title A typed',
        );
      },
    );

    test(
      'the app reports itself synced once the relay has everything',
      () async {
        final (_, a) = await open();
        await a.addItem('milk');
        await _untilStatus(
          a.watchSync(),
          () => a.syncStatus,
          (status) => status.state == SyncState.synced && status.pending == 0,
          'A never reported itself synced',
        );
        expect(a.syncStatus.lastError, isNull);
      },
    );

    test('an unreachable relay leaves the app working and says so', () async {
      final (library, a) = await open();
      // Nothing listens on this port; the app must carry on regardless.
      await library.connect('http://127.0.0.1:1');
      await a.addItem('milk');

      await _untilStatus(
        a.watchSync(),
        () => a.syncStatus,
        (status) => status.state == SyncState.offline && status.pending > 0,
        'A never noticed it could not reach the relay',
      );
      expect(a.current.items.single.text, 'milk');
      expect(a.syncStatus.lastError, isNotNull);
    });

    test('a device syncs every note it holds, not just the open one', () async {
      // The note list's half of sync: the library connects each note, including
      // one made after it connected, and its one status button is the sum of
      // them.
      final (library, groceries) = await open();
      await groceries.setTitle('Groceries');
      final hardwareId = await library.createNote();
      final hardware = library.openNote(hardwareId);
      addTearDown(hardware.dispose);
      await hardware.addItem('screws');

      await _untilStatus(
        library.watchSync(),
        () => library.syncStatus,
        (status) => status.state == SyncState.synced && status.pending == 0,
        'the device never reported every note synced',
      );

      // Both notes really are on the relay: a second device joining each one
      // gets its content, and the first knows nothing about the second.
      final (_, joinedHardware) = await share(library, hardwareId);
      await _until(
        joinedHardware.watch(),
        (view) => view.items.any((i) => i.text == 'screws'),
        'the second note never reached the relay',
      );
      expect(joinedHardware.current.title, isEmpty);
    });
  }, skip: skip);

  group("one person's two devices", () {
    test('a device adopted from a recovery phrase gets the notes and the '
        'placements', () async {
      // The slice's definition of done, point 2, from the app's own API.
      // `relay/tests/account.rs` holds it over cores and engines; this is the
      // same claim with the bridge, the library and a real Keystore-shaped
      // master key in the way.
      final a = await device();
      final phrase = await a.createAccount();
      expect(phrase, hasLength(12));
      expect(a.hasAccount, isTrue);

      final noteId = await a.createNote();
      final groceries = a.openNote(noteId);
      addTearDown(groceries.dispose);
      await groceries.setTitle('Groceries');
      await groceries.addItem('milk');
      await a.setColour(noteId, NoteColour.teal);
      await a.setPinned(noteId, true);

      // B has never seen a share blob. Twelve words is all it is given.
      final b = await device();
      expect(b.hasAccount, isFalse);
      await b.adoptAccount(phrase.join(' '));
      expect(b.hasAccount, isTrue);

      await _untilNotes(
        b.watchNotes(),
        (notes) => notes.any(
          (note) =>
              note.id == noteId &&
              note.title == 'Groceries' &&
              note.colour == NoteColour.teal &&
              note.pinned,
        ),
        "B never received A's note with A's placements on it",
      );

      // And it is the note rather than a card of one: B can open it and read
      // what is inside, which means the key travelled too.
      final bGroceries = b.openNote(noteId);
      addTearDown(bGroceries.dispose);
      await _until(
        bGroceries.watch(),
        (view) => view.items.any((item) => item.text == 'milk'),
        'B never received the note contents',
      );
    });

    test('a note deleted for good on one device leaves the other, and the '
        'rest keep syncing', () async {
      // ADR 0022 from the app's own API. `relay/tests/deletion.rs` holds it
      // over cores and engines; this adds the bridge's half — the account
      // settling in `Peer::settled`, and the deleted note's connection being
      // retired rather than left retrying for ever.
      final a = await device();
      final phrase = await a.createAccount();
      final goneId = await a.createNote();
      final gone = a.openNote(goneId);
      addTearDown(gone.dispose);
      await gone.addItem('milk');
      final keptId = await a.createNote();
      final kept = a.openNote(keptId);
      addTearDown(kept.dispose);
      await kept.addItem('drill bits');

      final b = await device();
      await b.adoptAccount(phrase.join(' '));
      await _untilNotes(
        b.watchNotes(),
        (notes) =>
            notes.any((note) => note.id == goneId) &&
            notes.any((note) => note.id == keptId),
        "B never received A's notes",
      );
      // Open on B, as it would be on a phone left on the list.
      final bGone = b.openNote(goneId);
      addTearDown(bGone.dispose);
      await _until(
        bGone.watch(),
        (view) => view.items.any((item) => item.text == 'milk'),
        'B never received the note contents',
      );

      await a.setTrashed(goneId, true);
      await a.forgetNote(goneId);

      await _untilNotes(
        b.watchNotes(),
        (notes) =>
            notes.every((note) => note.id != goneId) &&
            notes.any((note) => note.id == keptId),
        'B never let go of the deleted note',
      );
      expect(await b.search('milk'), isEmpty);
      await _untilStatus(
        b.watchSync(),
        () => b.syncStatus,
        (status) => status.state == SyncState.synced && status.pending == 0,
        'B never settled after the deletion',
      );

      // The note that was kept still syncs, both ways, on the same socket.
      final bKept = b.openNote(keptId);
      addTearDown(bKept.dispose);
      await bKept.addItem('screws');
      await _until(
        kept.watch(),
        (view) => view.items.any((item) => item.text == 'screws'),
        "A never received B's edit to the kept note",
      );
    });

    test(
      'a phrase that is not a phrase is refused with something to say',
      () async {
        final library = await device();
        await expectLater(
          library.adoptAccount('milk bread eggs'),
          throwsA(anything),
        );
        expect(library.hasAccount, isFalse);
      },
    );
  }, skip: skip);

  group('sharing with another person by invite', () {
    /// A device that can share: an account, and a name.
    Future<RustNoteLibrary> person(String name) async {
      final library = await device();
      await library.createAccount();
      await library.setProfileName(name);
      return library;
    }

    /// An owner with a titled list, set up to share.
    Future<(RustNoteLibrary, NoteCore, String)> owned() async {
      final (owner, a) = await open();
      await owner.createAccount();
      await owner.setProfileName('Olga');
      return (owner, a, owner.notes.single.id);
    }

    test(
      'a guest opens a link, is let in without asking, and the list arrives',
      () async {
        final (owner, a, noteId) = await owned();
        await a.setTitle('Groceries');
        await a.addItem('oat milk');

        final invite = await owner.createInvite(noteId);
        expect(invite.link, startsWith('easynotes://join#'));
        expect(owner.invitesFor(noteId).single.id, invite.id);

        // Only the link crosses. It carries no key: the guest's library has
        // nothing to open until the owner's device lets them in.
        final guest = await person('Anna');
        final waiting = await guest.requestJoin(invite.link);
        expect(waiting.name, 'Anna');
        expect(guest.pendingJoins.single.inviteId, waiting.inviteId);
        expect(
          (await guest.checkJoin(waiting.inviteId)).state,
          JoinState.waiting,
        );
        expect(guest.notes, isEmpty);

        expect(await letIn(owner, invite), 'Anna');
        expect(owner.invitesFor(noteId), isEmpty);
        expect(owner.people(noteId).people.map((p) => p.name), [
          'Olga',
          'Anna',
        ]);
        expect(owner.people(noteId).canManage, isTrue);

        final check = await joined(guest, waiting.inviteId);
        expect(check.state, JoinState.joined);
        expect(check.noteId, noteId);
        expect(guest.pendingJoins, isEmpty);

        final b = guest.openNote(check.noteId!);
        addTearDown(b.dispose);
        await _until(
          b.watch(),
          (view) =>
              view.title == 'Groceries' &&
              view.items.map((i) => i.text).join(',') == 'oat milk',
          'the guest was let in and never received the list',
        );
        final people = guest.people(noteId);
        expect(people.canManage, isFalse);
        expect(people.people.where((p) => p.me).single.name, 'Anna');
        await _untilNotes(
          guest.watchNotes(),
          (notes) => notes.any(
            (n) =>
                n.id == noteId &&
                n.collaborators.map((c) => c.name).contains('Olga'),
          ),
          'the guest cannot find the note by the person who shared it',
        );

        // And it is a membership, not a copy: an edit goes back.
        await b.setChecked(b.current.items.single.id, true);
        await _until(
          a.watch(),
          (view) => view.items.single.checked,
          'the owner never saw the guest tick an item',
        );

        // Removed: the note leaves the guest's device, with one message, and
        // the owner carries on.
        final anna = owner.people(noteId).people.firstWhere((p) => !p.owner);
        await owner.removePerson(noteId, anna);
        await _untilNotes(
          guest.watchNotes(),
          (notes) => notes.every((n) => n.id != noteId),
          'the removed guest kept the note',
        );
        expect(guest.takeRemovals().single.title, 'Groceries');
        await a.setTitle('Groceries, just us');
        await _until(
          a.watch(),
          (view) => view.title == 'Groceries, just us',
          'the owner could not carry on',
        );
      },
    );

    test("joining adds a note, and not the owner's colour or labels", () async {
      // A colour and a label are one person's filing, kept in their account
      // document; the note everyone shares is not where they go. And a guest
      // who already has notes keeps them: a join adds, it does not replace.
      final (owner, a, noteId) = await owned();
      await a.setTitle('Groceries');
      await owner.setColour(noteId, NoteColour.pink);
      await owner.addTag(noteId, 'errands');

      final guest = await person('Anna');
      final mine = guest.openNote(await guest.createNote());
      addTearDown(mine.dispose);
      await mine.setTitle('Mine');

      final invite = await owner.createInvite(noteId);
      final waiting = await guest.requestJoin(invite.link);
      await letIn(owner, invite);
      expect((await joined(guest, waiting.inviteId)).state, JoinState.joined);

      final b = guest.openNote(noteId);
      addTearDown(b.dispose);
      await _until(
        b.watch(),
        (view) => view.title == 'Groceries',
        'the guest never received the note',
      );
      expect(
        guest.notes.map((n) => n.title),
        containsAll(['Mine', 'Groceries']),
      );
      final shared = guest.notes.singleWhere((n) => n.id == noteId);
      expect(
        shared.colour,
        isNull,
        reason: "one person's colour reached another person's device",
      );
      expect(
        shared.tags,
        isEmpty,
        reason: "one person's label reached another person's device",
      );
      expect(guest.tags, isEmpty);
    });

    test('a link works once, for the first account that opens it', () async {
      final (owner, _, noteId) = await owned();
      final invite = await owner.createInvite(noteId);
      final first = await person('Anna');
      final waiting = await first.requestJoin(invite.link);

      // Claimed the moment Anna opened it, before the owner's device has
      // seen anything.
      final late = await person('Late');
      await expectLater(
        late.requestJoin(invite.link),
        throwsA(contains('already been used')),
      );
      // Anna opening it again is the same request, not a refusal.
      expect((await first.requestJoin(invite.link)).inviteId, waiting.inviteId);

      expect(await letIn(owner, invite), 'Anna');
      expect((await joined(first, waiting.inviteId)).state, JoinState.joined);
      expect(owner.people(noteId).people.map((p) => p.name), ['Olga', 'Anna']);
      await expectLater(
        late.requestJoin(invite.link),
        throwsA(contains('already been used')),
      );
    });

    test(
      'the dialog and the grid both see who joined, at once or after',
      () async {
        // The share dialog and the banners above the grid poll the same
        // invite on timers of their own. The check that lets someone in also
        // forgets the invite, so two at once used to answer the relay twice,
        // and one of them said there was no such invite instead of who joined.
        final (owner, _, noteId) = await owned();
        final invite = await owner.createInvite(noteId);
        final guest = await person('Anna');
        await guest.requestJoin(invite.link);
        final deadline = DateTime.now().add(patience);
        while (true) {
          final both = await Future.wait([
            owner.checkInvite(invite.id),
            owner.checkInvite(invite.id),
          ]);
          expect(both.last.joined, both.first.joined);
          if (both.first.joined != null) {
            expect(both.first.joined, 'Anna');
            break;
          }
          if (DateTime.now().isAfter(deadline)) fail('nobody was let in');
          await Future<void>.delayed(const Duration(milliseconds: 50));
        }
        // And whichever asks after the join is done hears the same, rather
        // than that the invite it was showing does not exist.
        expect((await owner.checkInvite(invite.id)).joined, 'Anna');
        expect(owner.people(noteId).people.map((p) => p.name), [
          'Olga',
          'Anna',
        ]);
      },
    );

    test('a withdrawn link is gone for the guest waiting on it', () async {
      final (owner, _, noteId) = await owned();
      final invite = await owner.createInvite(noteId);
      final guest = await person('Anna');
      final waiting = await guest.requestJoin(invite.link);

      await owner.withdrawInvite(invite.id);

      final check = await guest.checkJoin(waiting.inviteId);
      expect(check.state, JoinState.gone);
      expect(check.message, contains('does not exist'));
      expect(guest.pendingJoins, isEmpty);
    });

    test('sharing asks for an account and a name first', () async {
      final (owner, _) = await open();
      await expectLater(
        owner.createInvite(owner.notes.single.id),
        throwsA(contains('no account')),
      );
      await owner.createAccount();
      await expectLater(
        owner.createInvite(owner.notes.single.id),
        throwsA(contains('name')),
      );
    });
  }, skip: skip);
}

Future<void> _untilNotes(
  Stream<List<NoteSummary>> lists,
  bool Function(List<NoteSummary>) ready,
  String complaint,
) async {
  try {
    await lists.firstWhere(ready).timeout(patience);
  } on TimeoutException {
    fail(complaint);
  }
}

/// The address the relay logged. Reading it beats guessing a port, and lets the
/// OS pick one so tests never collide.
Future<String> _listeningAddress(Process relay) async {
  final line = await relay.stdout
      .transform(utf8.decoder)
      .transform(const LineSplitter())
      // The log is decorated for a terminal; strip that before matching.
      .map((l) => l.replaceAll(RegExp(r'\x1B\[[0-9;]*m'), ''))
      .firstWhere((l) => l.contains('relay listening'))
      .timeout(patience);
  final addr = RegExp(r'addr=([\d.]+:\d+)').firstMatch(line);
  if (addr == null) {
    throw StateError('could not read the relay address from: $line');
  }
  return addr.group(1)!;
}

Future<void> _until(
  Stream<NoteView> views,
  bool Function(NoteView) ready,
  String complaint,
) async {
  try {
    await views.firstWhere(ready).timeout(patience);
  } on TimeoutException {
    fail(complaint);
  }
}

Future<void> _untilStatus(
  Stream<SyncStatus> statuses,
  SyncStatus Function() current,
  bool Function(SyncStatus) ready,
  String complaint,
) async {
  try {
    await statuses.firstWhere(ready).timeout(patience);
  } on TimeoutException {
    fail('$complaint (last status: ${current()})');
  }
}
