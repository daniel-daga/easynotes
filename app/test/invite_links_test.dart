import 'package:easynotes/core/invite_links.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  const secret = 'ASRciIVJtU8m60KIfMaTLg1HrJI9dhPOzsPW5gQdPigp';
  const native = 'easynotes://join#$secret';

  test('a web link keeps its second # and drops the page query', () {
    final link = inviteLinkForSharing(
      native,
      webOrigin: 'http://127.0.0.1:8099/app/?lang=de#/notes',
      web: true,
    );
    expect(link, 'http://127.0.0.1:8099/app/#join#$secret');
  });

  test('a web link reads back to the same invite, opened or pasted', () {
    final link = inviteLinkForSharing(
      native,
      webOrigin: 'https://notes.example/',
      web: true,
    );
    expect(inviteLinkFrom(link), native);
    // What a browser's address bar hands back: the second # encoded.
    expect(inviteLinkFrom('https://notes.example/#join%23$secret'), native);
    // A pasted message reaches the core whole, and it reads after the last #.
    final message = 'Join my shared note in EasyNotes: $link';
    expect(message.substring(message.lastIndexOf('#') + 1), secret);
  });

  test('a native build shares the link as the core made it', () {
    expect(inviteLinkForSharing(native, webOrigin: 'https://x/'), native);
  });
}
