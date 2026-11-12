/// The bridge's machine-readable codes for refusals Dart has to act on.
///
/// A code sits between markers no message uses, in front of the text, so that
/// rewording a message cannot silently change what the app does. The other half
/// is `coded` in `app/rust/src/api/notes.rs`.
library;

const _marker = '\u0001';

/// [error]'s code, or null for an error the bridge did not classify. The
/// markers are looked for anywhere because a fake may wrap the text in an
/// exception, whose `toString` adds its own prefix.
String? bridgeErrorCode(Object error) {
  final text = '$error';
  final start = text.indexOf(_marker);
  if (start < 0) return null;
  final end = text.indexOf(_marker, start + 1);
  return end < 0 ? null : text.substring(start + 1, end);
}

/// [error]'s message without the bridge's code, for showing to a person.
String bridgeErrorText(Object error) {
  final text = '$error';
  final start = text.indexOf(_marker);
  if (start < 0) return text;
  final end = text.indexOf(_marker, start + 1);
  return end < 0 ? text : text.substring(end + 1);
}

/// An error shaped like one the bridge hands over, for a fake.
String bridgeError(String code, String message) =>
    '$_marker$code$_marker$message';

/// The profile's library has been closed by a switch or a shutdown.
bool isLibraryClosed(Object error) =>
    bridgeErrorCode(error) == 'library_closed';
