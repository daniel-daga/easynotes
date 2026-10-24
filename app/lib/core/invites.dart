/// Sharing a note with other people, by invite link.
///
/// A collaborator is a person, not a phone: their account, under the name they
/// chose. The link carries a secret and no key. The first account to open it
/// claims it, and the owner's device lets them in without asking anyone; the
/// link then stops working for everybody else. The owner can remove one
/// person, and everyone else keeps the note. An invite lasts seven days. See
/// `docs/adr/0025-links-that-let-people-in.md` and
/// `docs/adr/0023-access-packages.md`.
///
/// Every call that says "relay" needs one: an invite lives there between the
/// two people. What this device remembers about its invites and its requests
/// survives a restart, so either side may close the app before the other's
/// device has caught up.
library;

import 'package:flutter/foundation.dart';

String _initials(String name) => name.trim().isEmpty
    ? '?'
    : name
          .trim()
          .split(RegExp(r'\s+'))
          .take(2)
          .map((s) => String.fromCharCode(s.runes.first))
          .join()
          .toUpperCase();

/// Somebody a search can filter by: one entry per person.
@immutable
class Collaborator {
  const Collaborator({required this.id, required this.name});

  /// The person's member ID. Two people with one name are two of these.
  final String id;
  final String name;
  String get displayName => name.isEmpty ? 'Unnamed person' : name;
  String get initials => _initials(name);
  @override
  bool operator ==(Object other) =>
      other is Collaborator && id == other.id && name == other.name;
  @override
  int get hashCode => Object.hash(id, name);
}

/// One person with access to a note, as the share screen lists them.
@immutable
class Person {
  const Person({
    required this.member,
    required this.invite,
    required this.name,
    this.owner = false,
    this.me = false,
    this.removing = false,
  });

  /// One per person, however many phones they have.
  final String member;

  /// The invite that let them in. Empty for the owner.
  final String invite;

  /// Chosen by the person. A label, not a verified identity.
  final String name;
  final bool owner;

  /// The person looking at the screen.
  final bool me;

  /// Asked to be removed, and not yet: the removal finishes when this device
  /// is online. Nothing claims access is gone before then.
  final bool removing;

  String get displayName => name.isEmpty ? 'Unnamed person' : name;
  String get initials => _initials(name);
}

/// Everybody with access to a note, and who is on their way in.
@immutable
class People {
  const People({
    this.shared = false,
    required this.canManage,
    this.people = const [],
    this.adding = const [],
  });

  /// Whether anyone but the owner has been let in.
  final bool shared;

  /// Only the owner adds and removes people.
  final bool canManage;
  final List<Person> people;

  /// Names of people let in whose access is still on its way.
  final List<String> adding;
}

/// An invite this device has open.
@immutable
class Invite {
  const Invite({
    required this.id,
    required this.noteId,
    required this.link,
    required this.expiresIn,
  });

  final String id;
  final String noteId;

  /// What to send. It holds no key, but the first person to open it is let
  /// in.
  final String link;

  /// By this device's clock. The relay's is the one that holds.
  final Duration expiresIn;

  @override
  bool operator ==(Object other) =>
      other is Invite &&
      other.id == id &&
      other.noteId == noteId &&
      other.link == link;

  @override
  int get hashCode => Object.hash(id, noteId, link);
}

/// Where an invite stands, from the relay.
@immutable
class InviteStatus {
  const InviteStatus({
    required this.open,
    required this.expiresIn,
    this.lettingIn,
    this.joined,
  });

  /// False once somebody has opened the link or it has expired.
  final bool open;
  final Duration expiresIn;

  /// Somebody who opened the link, whose access is still on its way.
  final String? lettingIn;

  /// Somebody who opened the link and is now in. Reported once.
  final String? joined;
}

/// A request this device made and is waiting on.
@immutable
class PendingJoin {
  const PendingJoin({
    required this.inviteId,
    required this.name,
    this.noteId,
  });

  final String inviteId;
  final String name;

  /// The note the link's pre-grant opened, when it did
  /// ([ADR 0030](../../../docs/adr/0030-a-link-lets-its-first-person-in.md)).
  /// The request is still answered later: an approval adds this person to the
  /// package, and a decline takes the note away again.
  final String? noteId;

  /// Whether the note can be read already, so no waiting dialog is needed.
  bool get readable => noteId != null;
}

enum JoinState {
  /// The owner's device has not seen the request yet.
  waiting,

  /// Let in, and the note is still arriving.
  opening,

  /// Let in. The note is on this device.
  joined,

  /// Turned away: the owner's device could not read the request.
  declined,

  /// Expired, used by someone else, or withdrawn.
  gone,
}

@immutable
class JoinCheck {
  const JoinCheck(this.state, {this.noteId, this.message});

  final JoinState state;

  /// The note, once let in.
  final String? noteId;

  /// The relay's words, for [JoinState.gone].
  final String? message;
}

/// A note this device was removed from, for the one message it shows.
@immutable
class Removal {
  const Removal({required this.noteId, required this.title});
  final String noteId;
  final String title;

  String get message => title.isEmpty
      ? 'You no longer have access to a shared note.'
      : 'You no longer have access to ‘$title’.';
}

/// The person using this device: twelve words, and a name.
abstract class Accounts {
  /// Whether this device belongs to an account, and so whether it has any way
  /// to reach this person's other devices — or to share at all.
  bool get hasAccount;

  /// Mint an account for this device and return its recovery phrase, word by
  /// word, once. The caller must make somebody write it down.
  Future<List<String>> createAccount();

  /// Adopt the account twelve words name, keeping what this device already
  /// has.
  Future<void> adoptAccount(String phrase);

  /// The name this person shares under, once chosen. Asked once, as part of
  /// setting up sharing, and the same on every device of the account.
  String? get profileName;

  Future<void> setProfileName(String name);

  /// Delete this person's account from the relay and everything from this
  /// device ([ADR 0026](../../../docs/adr/0026-deleting-an-account.md)): the
  /// lists they own leave the relay, the ones they joined stay their owners',
  /// and this profile is left empty, to be opened again as a new device.
  ///
  /// Throws, having changed nothing here, when the relay cannot be reached or
  /// refuses; trying again is safe.
  Future<void> deleteAccount();
}

/// One-time transfer between two of a person's devices. Secret material stays
/// inside the Rust core; Dart receives only the relay settings after decryption.
abstract interface class AccountTransfers {
  Future<String> beginAccountTransfer(String relayUrl, String? relayAccess);
  Future<String?> accountTransferRequestCode();
  Future<void> approveAccountTransfer();
  Future<void> cancelAccountTransfer();
  Future<String> beginAccountTransferFromQr(String qr);
  Future<({String relayUrl, String? relayAccess})?> pollAccountTransferResult();
  Future<void> adoptAccountTransfer();
}

/// What a device can do about sharing, on either side of it.
abstract class Invites implements Accounts {
  /// Everybody with access to a note, owner first.
  People people(String noteId);

  /// Open an invite to a note and return it, link and all.
  Future<Invite> createInvite(String noteId);

  /// The invites open to one note, oldest first. Local.
  List<Invite> invitesFor(String noteId);

  /// Every invite open on this device. Local.
  List<Invite> get allInvites;

  /// Where an invite stands. Asks the relay, and lets in whoever has opened
  /// the link — nobody is asked — finishing on a later call what cannot
  /// finish yet.
  Future<InviteStatus> checkInvite(String inviteId);

  /// Take an invite down. The link stops working.
  Future<void> withdrawInvite(String inviteId);

  /// Remove one person from a note. Everyone else keeps it.
  Future<void> removePerson(String noteId, Person person);

  /// Remove everyone but the owner, and cancel every open link to the note.
  Future<void> stopSharing(String noteId);

  /// Claim [link] under this account's name. The owner's device lets this
  /// person in the next time it is online.
  Future<PendingJoin> requestJoin(String link);

  /// The requests this device is waiting on. Local.
  List<PendingJoin> get pendingJoins;

  /// Ask how a request went. One that was let in adds the note.
  Future<JoinCheck> checkJoin(String inviteId);

  /// Stop waiting on a request.
  Future<void> cancelJoin(String inviteId);

  /// The notes this device was just removed from, each once.
  List<Removal> takeRemovals();
}

/// How the owner hears that somebody joined one of their notes when they are
/// not looking at its collaborators screen: a notification from the system,
/// on the device that let them in.
abstract class JoinAlerts {
  /// Ask to be allowed to notify. Called when the owner sends a link, which
  /// is the moment the question makes sense, and never before.
  Future<void> requestPermission();

  /// [name] was just let into [noteId] through [inviteId]. Called once per
  /// person.
  Future<void> joined({
    required String inviteId,
    required String noteId,
    required String name,
  });
}

/// The words of that notification.
({String title, String body}) joinedNotificationText(
  String name,
  String noteTitle,
) {
  final who = name.trim().isEmpty ? 'Someone' : name.trim();
  final what = noteTitle.trim();
  return (
    title: what.isEmpty ? '$who joined a shared note' : '$who joined $what',
    body: 'They opened your link and can see and edit the note now.',
  );
}
