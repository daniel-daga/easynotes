import 'dart:typed_data';

/// The native path uses `compute`; this exists only so the conditional
/// import compiles where there is no browser.
Future<Uint8List?> makeWebThumbnail(Uint8List bytes) async => null;
