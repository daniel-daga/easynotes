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

import 'package:easynotes/core/attachment_files.dart';
import 'package:easynotes/core/master_key.dart';
import 'package:easynotes/core/note_core.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/core/profile_library.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/relay_session.dart';
import 'package:easynotes/core/rust_note_core.dart';
import 'package:easynotes/core/rust_note_library.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;

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
  // A second relay, for the relay-profile tests: a person who switches relay
  // must find the first one's notes neither sent to it nor lost.
  Process? otherRelay;
  late String otherRelayUrl;

  setUpAll(() async {
    if (skip != null) return;
    await RustNoteCore.ensureInitialized(
      library: ExternalLibrary.open(libPath!),
    );
    (relay, relayUrl) = await _startRelay(relayPath!);
    (otherRelay, otherRelayUrl) = await _startRelay(relayPath);
  });

  tearDownAll(() {
    // Not guarded by `skip`: if `setUpAll` failed after starting a relay, this
    // is the only thing that will stop it.
    relay?.kill();
    otherRelay?.kill();
  });

  /// A device: its own directory, its own library, connected to the relay the
  /// way the app connects one — or to [relay] instead, when a test wants
  /// another.
  Future<RustNoteLibrary> device({String? relay}) async {
    final dir = await Directory.systemTemp.createTemp('easynotes-relay-test');
    final library = await RustNoteLibrary.open(
      dir.path,
      MasterKeyStore.newKey(),
    );
    addTearDown(() async {
      await library.dispose();
      await dir.delete(recursive: true);
    });
    await library.connect(relay ?? relayUrl);
    return library;
  }

  /// A device with one new note on it, which is what most of these want.
  Future<(RustNoteLibrary, NoteCore)> open({String? relay}) async {
    final library = await device(relay: relay);
    final core = await library.openNote(await library.createNote());
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
    final core = await guest.openNote(check.noteId!);
    addTearDown(core.dispose);
    return (guest, core);
  }

  group('attachments', () {
    /// Ask for an attachment's bytes until they are here, as a screen asks
    /// again when the transfers say they arrived.
    Future<Uint8List> fetched(
      NoteCore core,
      String id, {
      bool thumbnail = false,
    }) async {
      final deadline = DateTime.now().add(patience);
      while (true) {
        final bytes = await core.readAttachment(id, thumbnail: thumbnail);
        if (bytes != null) return bytes;
        if (DateTime.now().isAfter(deadline)) {
          fail('the attachment never arrived');
        }
        await Future<void>.delayed(const Duration(milliseconds: 50));
      }
    }

    test(
      'a photo on a device with no relay is attached at once and kept',
      () async {
        final dir = await Directory.systemTemp.createTemp('easynotes-media');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();
        // A real picture, and the preview the app makes of it: the core
        // holds a thumbnail to its own limits, so this is also the check
        // that what `makeThumbnail` makes is one it takes.
        final photo = img.encodePng(
          img.Image(width: 1600, height: 1200)
            ..clear(img.ColorRgb8(200, 120, 40)),
        );
        final thumb = (await makeThumbnail(photo))!;
        final preview = img.decodeJpg(thumb)!;
        expect([preview.width, preview.height], [512, 384]);

        var library = await RustNoteLibrary.open(dir.path, key);
        final id = await library.createNote();
        var core = await library.openNote(id);
        final added = await core.importAttachment(
          'kitchen.png',
          photo,
          thumbnail: thumb,
        );
        expect(added.pending, isTrue);
        expect(added.kind, AttachmentKind.image);
        expect(added.width, 1600);
        expect(added.thumbnailWidth, 512);
        // No relay to wait for: the queue publishes it straight away.
        await _until(
          core.watch(),
          (view) => view.attachments.any((a) => a.id == added.id),
          'the import was never published into the note',
        );
        expect(await core.pendingAttachments(), isEmpty);
        expect(await core.readAttachment(added.id), photo);
        expect(await core.readAttachment(added.id, thumbnail: true), thumb);
        await core.dispose();
        await library.dispose();

        library = await RustNoteLibrary.open(dir.path, key);
        core = await library.openNote(id);
        expect(core.current.attachments.single.name, 'kitchen.png');
        expect(await core.readAttachment(added.id), photo);

        // Anything that is not a picture or a sound file is refused before a
        // byte of it is kept.
        await expectLater(
          core.importAttachment(
            'notes.txt',
            Uint8List.fromList(utf8.encode('plain words')),
          ),
          throwsA(anything),
        );
        await core.removeAttachment(added.id);
        await _until(
          core.watch(),
          (view) => view.attachments.isEmpty,
          'the attachment was never removed',
        );
        await core.dispose();
        await library.dispose();
      },
      skip: skip,
    );

    test(
      'a photo reaches the other person once uploaded, and leaves with its removal',
      () async {
        final (aLibrary, a) = await open();
        final (bLibrary, b) = await share(aLibrary, aLibrary.notes.single.id);
        final photo = _png(1600, 1200, (2 << 20) + 5);
        final thumb = _png(320, 240, 4000);

        final uploads = <AttachmentTransfer>[];
        final watching = aLibrary.watchTransfers().listen(
          (transfers) => uploads.addAll(transfers.where((t) => t.upload)),
        );
        addTearDown(watching.cancel);

        final added = await a.importAttachment(
          'market.png',
          photo,
          thumbnail: thumb,
        );
        await _until(
          b.watch(),
          (view) => view.attachments.any((x) => x.id == added.id),
          'B never saw the photo',
        );
        await _until(
          a.watch(),
          (view) => view.attachments.any((x) => x.id == added.id),
          'A never published its own photo',
        );
        expect(await a.pendingAttachments(), isEmpty);
        expect(uploads, isNotEmpty, reason: 'the queue said nothing');

        // B has the descriptor and not the bytes; asking fetches them.
        expect(await fetched(b, added.id, thumbnail: true), thumb);
        expect(await fetched(b, added.id), photo);
        final shown = b.current.attachments.single;
        expect(shown.name, 'market.png');
        expect(shown.thumbnailWidth, 320);

        await a.removeAttachment(added.id);
        await _until(
          b.watch(),
          (view) => view.attachments.isEmpty,
          'B still shows a removed photo',
        );
        await bLibrary
            .watchTransfers()
            .firstWhere((transfers) => transfers.isEmpty)
            .timeout(patience);
      },
      skip: skip,
    );
    test(
      'an import made offline waits for the relay rather than being published',
      () async {
        final (aLibrary, a) = await open();
        final (_, b) = await share(aLibrary, aLibrary.notes.single.id);
        await aLibrary.disconnect();

        final added = await a.importAttachment('later.png', _png(8, 8, 3000));
        // Long enough for a queue that wrongly treated this as a note with no
        // relay to have published it.
        await Future<void>.delayed(const Duration(seconds: 1));
        expect(a.current.attachments, isEmpty);
        expect((await a.pendingAttachments()).single.id, added.id);

        await aLibrary.connect(relayUrl);
        await _until(
          b.watch(),
          (view) => view.attachments.any((x) => x.id == added.id),
          'B never saw the photo made offline',
        );
      },
      skip: skip,
    );
  });

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
      // Nothing listens on this port; the app must carry on regardless.
      final (_, a) = await open(relay: 'http://127.0.0.1:1');
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
      final hardware = await library.openNote(hardwareId);
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
      final groceries = await a.openNote(noteId);
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
      final bGroceries = await b.openNote(noteId);
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
      final gone = await a.openNote(goneId);
      addTearDown(gone.dispose);
      await gone.addItem('milk');
      final keptId = await a.createNote();
      final kept = await a.openNote(keptId);
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
      final bGone = await b.openNote(goneId);
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
      final bKept = await b.openNote(keptId);
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

  group('relay profiles', () {
    // `docs/plan-relay-settings.md`, step 5: every relay has a profile of its
    // own, and moving between them moves nothing. `relay_session_test.dart`
    // holds the order of a switch against fakes; this runs it with real
    // stores, real keys and two real relays, opened the way the app opens
    // them.
    late String base;
    late RelayProfiles profiles;

    setUp(() async {
      FlutterSecureStorage.setMockInitialValues({});
      final dir = await Directory.systemTemp.createTemp('easynotes-profiles');
      base = dir.path;
      addTearDown(() => dir.delete(recursive: true));
      profiles = RelayProfiles();
      await profiles.load();
    });

    Future<NoteLibrary> openIn(
      RelayProfile profile, {
      bool connect = true,
      bool allowEphemeral = true,
    }) => openProfileLibrary(base, profile, connect: connect);

    Future<RelaySession> start(String relay) async {
      await profiles.select(relay);
      final session = RelaySession(
        profiles: profiles,
        library: await openIn(profiles.selected),
        profile: profiles.selected,
        open: openIn,
      );
      addTearDown(() => session.library.dispose());
      return session;
    }

    Future<void> synced(NoteLibrary library, String complaint) => _untilStatus(
      library.watchSync(),
      () => library.syncStatus,
      (status) => status.state == SyncState.synced && status.pending == 0,
      complaint,
    );

    test('switching relay and back keeps each relay\'s notes and account to '
        'itself', () async {
      final session = await start(relayUrl);
      final first = session.library;
      final phrase = await first.createAccount();
      final groceriesId = await first.createNote();
      final groceries = await first.openNote(groceriesId);
      await groceries.setTitle('Groceries');
      await groceries.addItem('milk');
      await synced(first, 'the first relay never took the note');
      // Made just before the switch, with no wait for the relay: it has to
      // reach the first relay in the end, and only that one.
      await groceries.addItem('bread');
      await groceries.dispose();

      await session.switchTo(otherRelayUrl);
      final second = session.library;
      expect(second, isNot(same(first)));
      expect(session.profile.url, otherRelayUrl);
      expect(second.notes, isEmpty);
      expect(second.hasAccount, isFalse);
      final hardwareId = await second.createNote();
      final hardware = await second.openNote(hardwareId);
      await hardware.setTitle('Hardware');
      await hardware.dispose();
      await synced(second, 'the second relay never took its own note');

      // The second relay was given nothing of the first profile's: the phrase
      // restores nothing there, not even the account.
      final stranger = await device(relay: otherRelayUrl);
      await stranger.adoptAccount(phrase.join(' '));
      await synced(stranger, 'the stranger never caught up');
      expect(stranger.notes, isEmpty);

      await session.switchTo('$relayUrl/');
      final back = session.library;
      expect(session.profile.url, relayUrl);
      expect(back.hasAccount, isTrue);
      expect(back.notes.map((n) => n.id), [groceriesId]);
      await synced(back, 'the first profile never caught up after the switch');

      // And the first relay has all of it, the line made before the switch
      // included, and nothing of the second profile's.
      final restored = await device();
      await restored.adoptAccount(phrase.join(' '));
      await _untilNotes(
        restored.watchNotes(),
        (notes) => notes.any((n) => n.id == groceriesId),
        'the first relay never gave the note back',
      );
      final copy = await restored.openNote(groceriesId);
      addTearDown(copy.dispose);
      await _until(
        copy.watch(),
        (view) => view.items.map((i) => i.text).toSet().containsAll({
          'milk',
          'bread',
        }),
        'the line made before the switch never reached the first relay',
      );
      expect(restored.notes.map((n) => n.id), isNot(contains(hardwareId)));
    });

    test('a restart opens the relay that was selected last', () async {
      final session = await start(relayUrl);
      final id = await session.library.createNote();
      await session.switchTo(otherRelayUrl);
      await session.switchTo(relayUrl);
      await session.library.dispose();

      final relaunched = RelayProfiles();
      await relaunched.load();
      expect(relaunched.selected.url, relayUrl);
      final library = await openIn(relaunched.selected);
      addTearDown(library.dispose);
      expect(library.notes.map((n) => n.id), [id]);
    });

    test(
      'a library will not be connected to a relay that is not its own',
      () async {
        final (library, _) = await open();
        await synced(library, 'the device never reached its relay');
        await expectLater(
          library.connect(otherRelayUrl),
          throwsA(contains('belong to the relay at $relayUrl')),
        );
        // Refused without being disconnected, and its own relay, however it is
        // spelt, is still fine.
        expect(library.syncStatus.state, SyncState.synced);
        await library.connect('${relayUrl.replaceFirst('http', 'ws')}/');
        await synced(library, 'the device never reconnected to its own relay');
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

        final b = await guest.openNote(check.noteId!);
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

    test(
      'deleting an account purges the lists it owns: the guest keeps '
      'a copy that no longer syncs, and the device starts again empty',
      () async {
        final dir = await Directory.systemTemp.createTemp('easynotes-delete');
        final key = MasterKeyStore.newKey();
        var owner = await RustNoteLibrary.open(dir.path, key);
        addTearDown(() async {
          await owner.dispose();
          await dir.delete(recursive: true);
        });
        await owner.connect(relayUrl);
        await owner.createAccount();
        await owner.setProfileName('Olga');
        final noteId = await owner.createNote();
        final a = await owner.openNote(noteId);
        await a.setTitle('Groceries');
        await a.dispose();
        final (guest, b) = await share(owner, noteId);
        await _until(
          b.watch(),
          (view) => view.title == 'Groceries',
          'the guest never received the list',
        );

        await owner.deleteAccount();

        bool told(SyncStatus s) =>
            s.lastError?.contains('deleted by its owner') ?? false;
        if (!told(b.syncStatus)) {
          await _untilStatus(
            b.watchSync(),
            () => b.syncStatus,
            told,
            'the guest was never told the list was deleted',
          );
        }
        expect(b.current.title, 'Groceries', reason: 'the guest keeps a copy');
        expect(guest.notes.map((n) => n.id), contains(noteId));

        // Opened again, as the app does: a new, empty device.
        await owner.dispose();
        owner = await RustNoteLibrary.open(dir.path, key);
        expect(owner.hasAccount, isFalse);
        expect(owner.notes, isEmpty);
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
      final mine = await guest.openNote(await guest.createNote());
      addTearDown(mine.dispose);
      await mine.setTitle('Mine');

      final invite = await owner.createInvite(noteId);
      final waiting = await guest.requestJoin(invite.link);
      await letIn(owner, invite);
      expect((await joined(guest, waiting.inviteId)).state, JoinState.joined);

      final b = await guest.openNote(noteId);
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

/// Start a relay on a port the OS picks, and return it with its address.
///
/// Port 0: the OS picks, the relay logs what it got, and tests can run in
/// parallel without fighting over a port.
Future<(Process, String)> _startRelay(String path) async {
  final started = await Process.start(
    path,
    const [],
    environment: {
      'RELAY_ADDR': '127.0.0.1:0',
      // Every device here is 127.0.0.1, and a public relay's per-address
      // limits are sized for one person, not for a whole suite of them.
      'RELAY_RATE_LIMIT': 'off',
      'RUST_LOG': 'easynotes_relay=info',
      'NO_COLOR': '1',
    },
  );
  return (started, 'http://${await _listeningAddress(started)}');
}

/// The address the relay logged. Reading it beats guessing a port, and lets the
/// OS pick one so tests never collide.
Future<String> _listeningAddress(Process relay) async {
  // Read to the end, not to the line wanted: a pipe nobody reads fills up, and
  // then the relay blocks on its next log line with every test still waiting.
  final listening = Completer<String>();
  relay.stdout
      .transform(utf8.decoder)
      .transform(const LineSplitter())
      // The log is decorated for a terminal; strip that before matching.
      .map((l) => l.replaceAll(RegExp(r'\x1B\[[0-9;]*m'), ''))
      .listen((l) {
        if (!listening.isCompleted && l.contains('relay listening')) {
          listening.complete(l);
        }
      });
  relay.stderr.drain<void>();
  final line = await listening.future.timeout(patience);
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

/// A PNG header claiming [w] × [h], then filler to [len] bytes. Nothing on
/// this path decodes pixels, so a header is all a picture has to be.
Uint8List _png(int w, int h, int len) {
  final bytes = Uint8List(len);
  const head = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D];
  bytes.setAll(0, [...head, ...ascii.encode('IHDR')]);
  final data = ByteData.sublistView(bytes);
  data.setUint32(16, w);
  data.setUint32(20, h);
  for (var i = 24; i < len; i++) {
    bytes[i] = (i * 13) % 251;
  }
  return bytes;
}
