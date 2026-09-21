/// Sharing a note with another person, by invite link.
///
/// The link carries a secret and no key. Someone who opens it asks to join; the
/// owner sees who asked, with six digits both screens show, and lets them in or
/// turns them away; only then is the note's key handed over, sealed to the one
/// person let in. An invite admits one person and lasts seven days. See
/// `docs/adr/0015-invites.md`.
///
/// Every call that says "relay" needs one: an invite lives there between the
/// two people. What this device remembers about its invites and its requests
/// survives a restart, so either side may close the app while the other
/// decides.
library;

import 'package:flutter/foundation.dart';

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
  });

  final String id;

  /// What they typed. Nothing checks it: it is their word.
  final String name;

  /// The six digits their own screen shows. The check a careful owner makes
  /// is that the two match.
  final String code;
  final JoinRequestState state;

  /// False for a request nobody holding the link made. It cannot be approved.
  final bool readable;
}

/// Where an invite stands, from the relay.
@immutable
class InviteRequests {
  const InviteRequests({
    required this.open,
    required this.expiresIn,
    required this.requests,
  });

  /// False once somebody has been let in or the invite has expired. A closed
  /// invite is forgotten by this device as soon as it is seen closed.
  final bool open;
  final Duration expiresIn;
  final List<JoinRequest> requests;

  List<JoinRequest> get pending => [
    for (final r in requests)
      if (r.state == JoinRequestState.pending) r,
  ];
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

  /// The note, once [JoinState.joined].
  final String? noteId;

  /// The relay's words, for [JoinState.gone].
  final String? message;
}

/// What a device can do about invites, on either side of one.
abstract class Invites {
  /// Open an invite to a note and return it, link and all.
  Future<Invite> createInvite(String noteId);

  /// The invites open to one note, oldest first. Local.
  List<Invite> invitesFor(String noteId);

  /// Every invite open on this device. Local.
  List<Invite> get allInvites;

  /// Who has asked to join through an invite. Asks the relay.
  Future<InviteRequests> inviteRequests(String inviteId);

  /// Let the person who made [requestId] in, and close the invite.
  Future<void> approveRequest(String inviteId, String requestId);

  /// Turn a request away. The invite stays open.
  Future<void> declineRequest(String inviteId, String requestId);

  /// Take an invite down. The link stops working.
  Future<void> withdrawInvite(String inviteId);

  /// Rotate the note key and remove everybody outside the owner's account.
  /// Copies they already decrypted cannot be recalled.
  Future<void> revokeCollaborators(String noteId);

  /// Ask to join through [link], going by [name] to the owner.
  Future<PendingJoin> requestJoin(String link, {String name = ''});

  /// The requests this device is waiting on. Local.
  List<PendingJoin> get pendingJoins;

  /// Ask how a request went. An approved one adds the note.
  Future<JoinCheck> checkJoin(String inviteId);

  /// Stop waiting on a request.
  Future<void> cancelJoin(String inviteId);
}
