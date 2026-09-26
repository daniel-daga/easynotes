/// Native Windows plugin smoke test, run by tools/windows-storage-smoke.ps1.
/// Each phase runs in a fresh process. No mocks and no production key or DB.
library;

import 'dart:convert';
import 'dart:io';

import 'package:easynotes/core/master_key.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/rust_note_library.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:path_provider/path_provider.dart';

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();
  if (!Platform.isWindows || args.length != 2) exit(2);
  final directory = Directory(args[1]);
  final name = directory.uri.pathSegments.where((s) => s.isNotEmpty).last;
  if (!RegExp(r'^easynotes-storage-smoke-[a-f0-9]{32}$').hasMatch(name)) {
    exit(2);
  }
  final keyName = '$name.master-key';
  const storage = FlutterSecureStorage();
  try {
    if (args[0] == 'cleanup') {
      await storage.delete(key: keyName);
      if (await storage.read(key: keyName) != null) {
        throw StateError('Test key survived deletion');
      }
    } else {
      // Exercise path_provider's native registration too, without using the
      // production application-support directory for this temporary database.
      if (!(await getApplicationSupportDirectory()).isAbsolute) {
        throw StateError('Application support path is not absolute');
      }
      if (args[0] == 'write') {
        if (await storage.read(key: keyName) != null) {
          throw StateError('Test key already exists');
        }
        await storage.write(
          key: keyName,
          value: base64Encode(MasterKeyStore.newKey()),
        );
      } else if (args[0] != 'read') {
        throw ArgumentError('Unknown phase');
      }
      final encoded = await storage.read(key: keyName);
      if (encoded == null) throw StateError('Windows did not retain the key');
      final key = base64Decode(encoded);
      if (key.length != masterKeyBytes) throw StateError('Invalid key length');
      final library = await RustNoteLibrary.open(directory.path, key);
      try {
        if (args[0] == 'write') {
          final id = await library.createNote(kind: NoteKind.text);
          final note = await library.openNote(id);
          try {
            await note.setTitle('Windows persistence check');
            await note.editBody('', 'Saved through the native Windows runner.');
          } finally {
            await note.dispose();
          }
        } else {
          if (library.notes.length != 1) throw StateError('Expected one note');
          final note = await library.openNote(library.notes.single.id);
          try {
            if (note.current.title != 'Windows persistence check' ||
                note.current.body !=
                    'Saved through the native Windows runner.') {
              throw StateError('Reopened note differs from the saved note');
            }
          } finally {
            await note.dispose();
          }
        }
      } finally {
        await library.dispose();
        key.fillRange(0, key.length, 0);
      }
    }
    await File('${directory.path}/result-${args[0]}.txt').writeAsString('ok');
    exit(0);
  } catch (error, stack) {
    // Never write the key to logs or the test directory.
    await File('${directory.path}/failure.txt').writeAsString('$error\n$stack');
    exit(1);
  }
}
