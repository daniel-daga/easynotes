/// Keeps a screen showing a secret out of screenshots and the Recents list.
///
/// Android does this with `FLAG_SECURE`; iOS has no equivalent screenshot
/// block, so the recovery phrase and the transfer QR remain capturable there.
library;

import 'secure_screen_stub.dart'
    if (dart.library.io) 'secure_screen_native.dart' as platform;

/// Marks the app's window as not capturable while a secret is on screen, and
/// unmarks it when the screen is gone.
// shortcut: one window flag, no count — a secret screen opened from another
// secret screen would clear it early; add a count when that flow exists.
Future<void> setSecureScreen(bool secure) => platform.setSecureScreen(secure);
