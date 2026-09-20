/// Where a note's sync stands, as the UI understands it.
///
/// The Dart mirror of `easynotes_core::sync::SyncStatus`. Like [NoteView] it is
/// a plain value the UI renders and never edits: the engine decides what is
/// true, the screen only says so.
library;

import 'package:flutter/foundation.dart';

enum SyncState {
  /// There is nothing to sync: this device holds no notes.
  ///
  /// Its own state rather than [offline] because the two look the same to a
  /// reader and mean opposite things — a fresh install used to say "Not
  /// syncing" with a perfectly healthy relay behind it.
  empty,

  /// Not connected. Everything still works — the relay just has not heard it.
  offline,

  connecting,

  /// Connected, still catching up on what arrived while we were away.
  online,

  /// Connected and caught up.
  synced,
}

@immutable
class SyncStatus {
  const SyncStatus({
    required this.state,
    this.pending = 0,
    this.lastSeq = 0,
    this.lastError,
  });

  const SyncStatus.offline() : this(state: SyncState.offline);

  final SyncState state;

  /// This device's edits the relay has not acknowledged. Worth showing: it is
  /// the difference between "saved here" and "saved anywhere else".
  final int pending;

  /// The relay sequence number this device has applied up to.
  final int lastSeq;

  /// Why the last attempt failed, if it did.
  final String? lastError;

  bool get isConnected =>
      state == SyncState.online || state == SyncState.synced;

  @override
  bool operator ==(Object other) =>
      other is SyncStatus &&
      other.state == state &&
      other.pending == pending &&
      other.lastSeq == lastSeq &&
      other.lastError == lastError;

  @override
  int get hashCode => Object.hash(state, pending, lastSeq, lastError);

  @override
  String toString() =>
      'SyncStatus($state, pending: $pending, seq: $lastSeq, error: $lastError)';
}
