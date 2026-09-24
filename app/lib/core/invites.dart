/// Sharing a note with other people, by invite link.
///
/// A collaborator is a person, not a phone: their account, under the name they
/// chose. The link carries a secret and no key. Someone who opens it asks to
/// join; the owner sees who asked, with six digits both screens show, and lets
/// them in with one tap. The owner can remove one person, and everyone else
/// keeps the note. An invite admits one person and lasts seven days. See
/// `docs/plan-account-collaboration.md` and `docs/adr/0023-access-packages.md`.
///
/// Every call that says "relay" needs one: an invite lives there between the
/// two people. What this device remembers about its invites and its requests
/// survives a restart, so either side may close the app while the other
/// decides.
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

  /// Names of people approved whose access is still on its way.
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

  /// What to send. It holds no key, but anyone who has it can *ask*.
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

enum JoinRequestState { pending, approved, declined }

/// Somebody asking to join, as the owner sees them.
@immutable
class JoinRequest {
  const JoinRequest({
    required this.id,
    required this.name,
    required this.code,
    required this.state,
    this.readable = true,
    this.alreadyMember = false,
  });

  final String id;

  /// The name their account shares under. Nothing checks it: it is their
  /// word.
  final String name;

  /// The six digits their own screen shows. The check a careful owner makes
  /// is that the two match. Optional.
  final String code;
  final JoinRequestState state;

  /// False for a request nobody holding the link made. It cannot be approved.
  final bool readable;

  /// This person already has access — from another of their phones, say.
  /// Approving changes nothing.
  final bool alreadyMember;
}

/// Where an invite stands, from the relay.
@immutable
class InviteRequests {
  const InviteRequests({
    required this.open,
    required this.expiresIn,
    required this.requests,
    this.lettingIn,
  });

  /// False once somebody has been let in or the invite has expired.
  final bool open;
  final Duration expiresIn;
  final List<JoinRequest> requests;

  /// Somebody approved whose access is still on its way.
  final String? lettingIn;

  List<JoinRequest> get pending => [
    for (final r in requests)
      if (r.state == JoinRequestState.pending) r,
  ];
}

/// Where an approval got to.
enum Approval {
  /// They are in.
  done,

  /// Saved, and on its way; it finishes when this device reaches the relay.
  pending,

  /// They already had access. Nothing changed.
  alreadyMember,
}

/// A request this device made and is waiting on.
@immutable
class PendingJoin {
  const PendingJoin({
    required this.inviteId,
    required this.name,
    required this.code,
  });

  final String inviteId;
  final String name;

  /// What the owner will see beside this request.
  final String code;
}

enum JoinState {
  /// Nobody has answered.
  waiting,

  /// Let in, and the note is still arriving.
  opening,

  /// Let in. The note is on this device.
  joined,

  /// Turned away.
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

  /// Who has asked to join through an invite. Asks the relay, and finishes an
  /// approval that was interrupted.
  Future<InviteRequests> inviteRequests(String inviteId);

  /// Let the person who made [requestId] in, and close the invite.
  Future<Approval> approveRequest(String inviteId, String requestId);

  /// Turn a request away. The invite stays open.
  Future<void> declineRequest(String inviteId, String requestId);

  /// Take an invite down. The link stops working.
  Future<void> withdrawInvite(String inviteId);

  /// Remove one person from a note. Everyone else keeps it.
  Future<void> removePerson(String noteId, Person person);

  /// Remove everyone but the owner, and cancel every open link to the note.
  Future<void> stopSharing(String noteId);

  /// Ask to join through [link], under this account's name.
  Future<PendingJoin> requestJoin(String link);

  /// The requests this device is waiting on. Local.
  List<PendingJoin> get pendingJoins;

  /// Ask how a request went. An approved one adds the note.
  Future<JoinCheck> checkJoin(String inviteId);

  /// Stop waiting on a request.
  Future<void> cancelJoin(String inviteId);

  /// The notes this device was just removed from, each once.
  List<Removal> takeRemovals();
}
