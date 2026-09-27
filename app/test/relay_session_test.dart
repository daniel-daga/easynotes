/// Moving from one relay's profile to another's, against fakes.
///
/// `relay_sync_test.dart` switches between two real relays; this holds the
/// order the switch happens in, and what it does when a step fails, which a
/// real store cannot be made to fail on cue.
library;

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/relay_session.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

/// A library whose dispose is recorded, and can be told to lose a write on
/// the way out.
class _Library extends FakeNoteLibrary {
  _Library(this.name, {super.canJoin});

  final String name;
  bool disposed = false;
  String? refuseOnDispose;

  @override
  Future<void> dispose() async {
    calls.add('dispose()');
    disposed = true;
    if (refuseOnDispose != null) pushStorageError(refuseOnDispose!);
    await super.dispose();
  }
}

/// Profiles whose writes can be made to fail.
class _Profiles extends RelayProfiles {
  bool failActivate = false;

  @override
  Future<void> activate(RelayProfile profile) async {
    if (failActivate) {
      failActivate = false;
      throw StateError('keychain refused');
    }
    await super.activate(profile);
  }
}

void main() {
  late _Profiles profiles;
  late List<String> events;
  late List<_Library> opened;
  Object? failOpen;

  setUp(() async {
    FlutterSecureStorage.setMockInitialValues({});
    profiles = _Profiles();
    await profiles.load();
    events = [];
    opened = [];
    failOpen = null;
  });

  Future<NoteLibrary> open(
    RelayProfile profile, {
    bool connect = true,
    bool allowEphemeral = true,
  }) async {
    events.add('open(${profile.url}, connect: $connect)');
    if (failOpen != null) throw failOpen!;
    final library = _Library(profile.url ?? 'local');
    if (connect && profile.url != null) await library.connect(profile.url!);
    opened.add(library);
    return library;
  }

  RelaySession session(_Library library) {
    final session = RelaySession(
      profiles: profiles,
      library: library,
      profile: profiles.selected,
      open: open,
    );
    session.onRelease = (l) async =>
        events.add('release(${(l as _Library).name})');
    session.onInstall = (l) => events.add('install(${(l as _Library).name})');
    return session;
  }

  test('a switch opens the target, retires the old library, then connects '
      'the new one', () async {
    final local = _Library('local');
    final s = session(local);
    await s.switchTo('wss://Relay.Example/');

    expect(s.profile.url, 'https://relay.example');
    expect(s.library, same(opened.single));
    expect(events, [
      'open(https://relay.example, connect: false)',
      'release(local)',
      'install(https://relay.example)',
    ]);
    // Kept before it was closed; closed before the next one connected.
    expect(local.calls, containsAllInOrder(['flush()', 'dispose()']));
    expect(opened.single.calls, ['connect(https://relay.example)']);
    expect(local.calls.where((c) => c.startsWith('connect')), isEmpty);

    final again = RelayProfiles();
    await again.load();
    expect(again.selected.url, 'https://relay.example');
  });

  test('switching back reopens the same profile', () async {
    final s = session(_Library('local'));
    final local = s.profile;
    await s.switchTo('https://relay.example');
    await s.switchTo(null);
    expect(s.profile.id, local.id);
    expect(profiles.profiles, hasLength(2));
  });

  test(
    'the relay already selected is not a switch, however it is spelt',
    () async {
      final s = session(_Library('local'));
      await s.switchTo('https://relay.example');
      events.clear();
      await s.switchTo('wss://relay.example:443/');
      expect(events, isEmpty);
    },
  );

  test('a second tap while switching does not start a second switch', () async {
    final s = session(_Library('local'));
    final first = s.switchTo('https://a.example');
    final second = s.switchTo('https://b.example');
    expect(s.switching, isTrue);
    await Future.wait([first, second]);
    expect(s.switching, isFalse);
    expect(s.profile.url, 'https://a.example');
    expect(events.where((e) => e.startsWith('open')), hasLength(1));
  });

  test('a tab with no storage is refused before anything is opened', () async {
    final tab = _Library('local', canJoin: false);
    final s = session(tab);
    await expectLater(s.switchTo('https://relay.example'), throwsStateError);
    expect(events, isEmpty);
    expect(tab.disposed, isFalse);
    expect(s.library, same(tab));
  });

  test('unsaved changes are refused before anything is opened', () async {
    final local = _Library('local')..pushStorageError('QuotaExceededError');
    final s = session(local);
    await expectLater(s.switchTo('https://relay.example'), throwsStateError);
    expect(events, isEmpty);
    expect(local.disposed, isFalse);
    expect(s.library, same(local));
  });

  test(
    'a target that will not open leaves the current library as it was',
    () async {
      final local = _Library('local');
      final s = session(local);
      failOpen = StateError('database is locked');
      await expectLater(s.switchTo('https://relay.example'), throwsStateError);
      expect(local.disposed, isFalse);
      expect(s.library, same(local));
      expect(events, ['open(https://relay.example, connect: false)']);
      final again = RelayProfiles();
      await again.load();
      expect(again.selected.url, isNull);
    },
  );

  test(
    'a selection that cannot be saved reopens the previous profile',
    () async {
      final local = _Library('local');
      final s = session(local);
      final previous = s.profile;
      profiles.failActivate = true;
      await expectLater(s.switchTo('https://relay.example'), throwsStateError);

      final target = opened.first;
      expect(target.disposed, isTrue);
      expect(target.calls.where((c) => c.startsWith('connect')), isEmpty);
      expect(local.disposed, isTrue);
      expect(s.profile.id, previous.id);
      expect(s.library, same(opened.last));
      expect(s.library, isNot(same(local)));
      expect(events.last, 'install(local)');
      final again = RelayProfiles();
      await again.load();
      expect(again.selected.id, previous.id);
    },
  );

  test('a release that fails keeps the library it was releasing', () async {
    final local = _Library('local');
    final s = session(local);
    s.onRelease = (_) async => throw StateError('alarm service refused');
    await expectLater(s.switchTo('https://relay.example'), throwsStateError);
    expect(local.disposed, isFalse);
    expect(s.library, same(local));
    expect(opened.single.disposed, isTrue);
    // Its reminders and alerts are handed back to it, and nothing was
    // opened a second time on its store.
    expect(events.last, 'install(local)');
    expect(events.where((e) => e.startsWith('open')), hasLength(1));
  });

  test('a write lost while closing keeps the person on that profile', () async {
    final local = _Library('local')..refuseOnDispose = 'QuotaExceededError';
    final s = session(local);
    final previous = s.profile;
    await expectLater(s.switchTo('https://relay.example'), throwsStateError);
    expect(s.profile.id, previous.id);
    final again = RelayProfiles();
    await again.load();
    expect(again.selected.id, previous.id);
    expect(opened.first.disposed, isTrue);
  });
}
