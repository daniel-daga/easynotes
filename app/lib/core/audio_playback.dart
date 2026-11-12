/// A decrypted audio file handed to the player, and how to let it go.
library;

import 'dart:typed_data';

import 'audio_playback_stub.dart'
    if (dart.library.io) 'audio_playback_io.dart'
    if (dart.library.js_interop) 'audio_playback_web.dart'
    as platform;
import 'audio_playback_types.dart';

export 'audio_playback_types.dart';

Future<PreparedAudio> prepareAudio(Uint8List bytes, String? mime) =>
    platform.prepareAudio(bytes, mime);
