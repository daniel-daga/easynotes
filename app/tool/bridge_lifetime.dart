// Browser-only diagnostic entrypoint; compile with dart compile js.
import 'dart:js_interop';
import 'package:easynotes/src/rust/api/notes.dart';
import 'package:easynotes/src/rust/frb_generated.dart';

@JS('lifetimeBatch')
external set lifetimeBatch(JSFunction value);
@JS('lifetimeReady')
external set lifetimeReady(JSBoolean value);

Future<void> main() async {
  await RustLib.init();
  final library = Library.openEphemeral();
  final id = await library.createNote(kind: NoteKindView.checklist);
  lifetimeBatch = ((JSNumber count) => batch(
    library,
    id,
    count.toDartInt,
  ).toJS).toJS;
  lifetimeReady = true.toJS;
}

Future<JSAny?> batch(Library library, String id, int count) async {
  for (var i = 0; i < count; i++) {
    final note = await library.note(id: id);
    final views = note.watch().listen((_) {});
    final sync = note.watchSync().listen((_) {});
    final edit = note.setTitle(title: 'iteration $i');
    note.view();
    note.syncStatus();
    await edit;
    await note.stopWatching();
    await views.cancel();
    await sync.cancel();
    // Leave the handle to the finalizer, as RustNoteCore.dispose does.
  }
  return null;
}
