import 'package:easynotes/core/invite_links.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  const secret = 'ASRciIVJtU8m60KIfMaTLg1HrJI9dhPOzsPW5gQdPigp';
  const native = 'easynotes://join#$secret';
  const relay = 'https://sync.example.com';
  const access = 'Zm9vYmFyLXJlbGF5LWtleQ';
  const withRelay = 'easynotes://join#relay=$relay;access=$access#$secret';

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

  test('a link names the relay it belongs to', () {
    final link = inviteLinkForSharing(
      native,
      webOrigin: 'https://x/',
      relay: (url: relay, access: access),
    );
    expect(link, withRelay);
    expect(relayFromInvite(link), (url: relay, access: access));
    // The core still reads the secret: it is after the last #.
    expect(link.substring(link.lastIndexOf('#') + 1), secret);
  });

  test('a web link carries the relay before its secret', () {
    final link = inviteLinkForSharing(
      native,
      webOrigin: 'https://notes.example/app/',
      relay: (url: relay, access: access),
      web: true,
    );
    final params = Uri.encodeComponent('relay=$relay;access=$access');
    expect(link, 'https://notes.example/app/#join#$params#$secret');
    expect(inviteLinkFrom(link), withRelay);
    expect(relayFromInvite(link), (url: relay, access: access));
  });

  test('a native share is the relay page, with only the secret', () {
    final link = inviteLinkForRelayPage(native, relay);
    expect(link, 'https://sync.example.com/#$secret');
    // One address, one fragment: nothing to split, and the origin is the
    // relay. The page adds the relay when it hands the fragment to the app.
    expect('https://'.allMatches(link).length, 1);
    expect(inviteLinkFrom(link), 'easynotes://join#relay=$relay#$secret');
    expect(relayFromInvite(link), (url: relay, access: null));
  });

  test('a percent-encoded handoff reads back to the same invite', () {
    // What the relay's join page sends to the app: one literal `#`, with the
    // secret's separator encoded inside the fragment. An OS that treats a
    // second `#` as a fragment boundary cannot drop the secret here.
    final handoff =
        'easynotes://join#${Uri.encodeComponent('relay=$relay')}'
        '%23$secret';
    expect(inviteLinkFrom(handoff), 'easynotes://join#relay=$relay#$secret');
    expect(relayFromInvite(handoff), (url: relay, access: null));
  });

  test('a relay page under a base path keeps that path', () {
    final link = 'https://notes.example/relay/#$secret';
    expect(
      inviteLinkFrom(link),
      'easynotes://join#relay=https://notes.example/relay#$secret',
    );
    expect(relayFromInvite(link), (
      url: 'https://notes.example/relay',
      access: null,
    ));
  });

  test('a relay with no key is still one encoded field', () {
    final link = inviteLinkForSharing(
      native,
      webOrigin: relay,
      relay: (url: relay, access: null),
      web: true,
    );
    expect(
      link,
      'https://sync.example.com/#join#${Uri.encodeComponent('relay=$relay')}'
      '#$secret',
    );
    expect(inviteLinkFrom(link), 'easynotes://join#relay=$relay#$secret');
  });

  test('a relay with no key is named on its own', () {
    final link = inviteLinkForSharing(
      native,
      webOrigin: 'https://x/',
      relay: (url: relay, access: null),
    );
    expect(link, 'easynotes://join#relay=$relay#$secret');
    expect(relayFromInvite(link), (url: relay, access: null));
  });

  test('a link made before invites carried a relay names none', () {
    expect(inviteLinkFrom(native), native);
    expect(relayFromInvite(native), isNull);
    expect(relayFromInvite('https://notes.example/#join%23$secret'), isNull);
  });

  test('a relaying link survives the way a browser encodes it', () {
    // `#` inside a fragment reaches a parser as `%23`, which is what the web
    // address bar does to a link it was handed whole.
    final encoded =
        'https://notes.example/#join%23relay%3Dhttps%3A%2F%2Fsync.example.com'
        '%3Baccess%3D$access%23$secret';
    // The relay's `:` and `/` were encoded by whoever wrote the link; a
    // percent-decoding parser brings them back before the fields are read.
    expect(inviteLinkFrom(encoded), withRelay);
    expect(relayFromInvite(encoded), (url: relay, access: access));
  });

  test('anything that is not an invite is not one', () {
    expect(inviteLinkFrom(''), isNull);
    expect(inviteLinkFrom('https://notes.example/'), isNull);
    expect(inviteLinkFrom('hello'), isNull);
    expect(relayFromInvite('hello'), isNull);
    // A page that routes with a fragment is not an invite on every load.
    expect(inviteLinkFrom('https://notes.example/#/notes'), isNull);
    expect(inviteLinkFrom('https://notes.example/#notes'), isNull);
  });
}
