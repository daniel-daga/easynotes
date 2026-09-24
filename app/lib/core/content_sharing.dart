import 'content_sharing_stub.dart'
    if (dart.library.io) 'content_sharing_native.dart'
    if (dart.library.js_interop) 'content_sharing_web.dart' as platform;

/// Opens the platform's share sheet when one is available.
///
/// Returns false when this platform has no share sheet; callers can still offer
/// copy-to-clipboard and a selectable text fallback.
Future<bool> shareContent(String text) => platform.shareContent(text);
