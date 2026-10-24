//! Sharing with people: access packages, as one device acts on them.
//!
//! [`crate::access`] is the format and the rule for which package is current.
//! This is what a device does with them: records every package it sees,
//! switches to the key the current one grants it, deletes its copy when the
//! current one no longer lists it, and — on the owner's devices — builds the
//! next package from what the owner asked for. See
//! [ADR 0023](../../../docs/adr/0023-access-packages.md), §5–7 in particular.
//!
//! Two moments matter, and they are kept apart on purpose. A package that
//! *arrives* is only recorded ([`NoteCore::apply_remote`]). What this device
//! does about the packages it has is decided when the note has *settled*
//! ([`NoteCore::settle_note`]): the engine says so once the relay's backlog for
//! the note is drained, and for every arrival after that. Deciding mid-backlog
//! would walk through states the owner never left the note in — a person
//! removed and invited again would be deleted halfway through the replay.

use super::*;
use crate::access::{
    self, Body, Entry, Ledger, Member, Membership, Package, MAX_MEMBERS, OWNER_INVITE,
};
use crate::account::MemberIdentity;
use crate::crdt::account::NoteRecord;
use crate::ids::InviteId;
use std::collections::{BTreeMap, BTreeSet};

/// What the sync layer should send for a note after this device acted on who
/// can read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outgoing {
    /// The note's key changed. Anything the engine is still holding back
    /// under the old key is in the new state already, and must not be sent
    /// under a key a removed person holds.
    pub rekey: bool,
    /// Send these now, in this order.
    pub envelopes: Vec<Vec<u8>>,
    /// This device was removed from the note and has deleted its copy.
    pub removed: bool,
}

impl Outgoing {
    pub fn is_empty(&self) -> bool {
        !self.rekey && !self.removed && self.envelopes.is_empty()
    }

    /// Follow this with `later`. A later change of key supersedes what was
    /// to be sent under the earlier one: its state includes all of it.
    pub fn then(&mut self, later: Outgoing) {
        if later.rekey {
            self.envelopes = later.envelopes;
            self.rekey = true;
        } else {
            self.envelopes.extend(later.envelopes);
        }
        self.removed |= later.removed;
    }
}

/// One person on a note, as the share screen lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// The member ID, base64url. One per person, however many phones.
    pub member: String,
    /// The invite that let them in, base64url. Empty for the owner.
    pub invite: String,
    pub name: String,
    pub owner: bool,
    /// This is the person looking at the screen.
    pub me: bool,
    /// A removal has been asked for and has not yet taken effect: “Removing…
    /// finishes when you're online”.
    pub removing: bool,
}

/// Everybody with access to a note, and what is on its way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct People {
    /// Whether anyone but the owner has ever been let in.
    pub shared: bool,
    /// Whether this person can add and remove people: only the owner can.
    pub can_manage: bool,
    pub people: Vec<Person>,
    /// Names of people approved whose package has not yet taken effect.
    pub adding: Vec<String>,
}

/// What the owner has asked for and not yet seen take effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Intent {
    Add {
        member: String,
        box_key: String,
        invite: String,
        name: String,
    },
    Remove {
        member: String,
        invite: String,
    },
}

fn b64(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(bytes)
}

impl Intent {
    fn add(member: &Member) -> Self {
        Intent::Add {
            member: member.member.encode(),
            box_key: b64(&member.box_key),
            invite: member.invite.encode(),
            name: member.name.clone(),
        }
    }

    fn remove(m: &Membership) -> Self {
        Intent::Remove {
            member: m.member.encode(),
            invite: m.invite.encode(),
        }
    }

    fn membership(&self) -> Result<Membership> {
        let (member, invite) = match self {
            Intent::Add { member, invite, .. } | Intent::Remove { member, invite } => {
                (member, invite)
            }
        };
        Ok(Membership {
            member: AuthorKey(decode_32(member)?),
            invite: InviteId::decode(invite)?,
        })
    }

    fn member(&self) -> Result<Option<Member>> {
        let Intent::Add {
            member,
            box_key,
            invite,
            name,
        } = self
        else {
            return Ok(None);
        };
        Ok(Some(Member {
            member: AuthorKey(decode_32(member)?),
            box_key: decode_32(box_key)?,
            owner: false,
            invite: InviteId::decode(invite)?,
            name: name.clone(),
        }))
    }
}

/// Whether an approval changed anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approved {
    /// A package adding them is on its way.
    Adding,
    /// This person already has access, under another invite; nothing changes.
    AlreadyHasAccess,
}

impl<D: NoteDoc, S: OpStore, K: Keyring> NoteCore<D, S, K> {
    // -- this person --------------------------------------------------------

    /// This person's member and box keys, which only an account has.
    pub fn member(&self) -> Result<MemberIdentity> {
        self.keyring
            .account_root()?
            .map(|root| root.member())
            .ok_or(CoreError::NoAccount)
    }

    /// The name this person shares under, once they have chosen one.
    pub fn profile_name(&self) -> Option<String> {
        self.placements.profile_name.clone()
    }

    /// Choose the name this person's requests carry, on every device of the
    /// account. A name already recorded in a note's package is not changed.
    pub fn set_profile_name(&mut self, name: &str) -> Result<()> {
        if !self.has_account() {
            return Err(CoreError::NoAccount);
        }
        let name = access::clip_name(name);
        if name.is_empty() {
            return Err(CoreError::NoProfileName);
        }
        self.apply_account(AccountEdit::SetProfileName {
            name: name.to_string(),
        })
    }

    // -- what is stored -----------------------------------------------------

    pub(super) fn record(&self, id: NoteId) -> Option<NoteRecord> {
        account::note_access_of(self.account.doc.entries()).remove(&id)
    }

    pub(super) fn ledger(&self, id: NoteId) -> Result<Ledger> {
        match self.store.get_meta(id, ACCESS)? {
            Some(raw) => Ledger::decode(&raw),
            None => Ok(Ledger::default()),
        }
    }

    fn put_ledger(&mut self, id: NoteId, ledger: &Ledger) -> Result<()> {
        self.store.put_meta(id, ACCESS, &ledger.encode())
    }

    fn intents(&self, id: NoteId) -> Result<Vec<Intent>> {
        match self.store.get_meta(id, ACCESS_INTENTS)? {
            Some(raw) => serde_json::from_slice(&raw)
                .map_err(|e| CoreError::Storage(format!("unreadable access intents: {e}"))),
            None => Ok(Vec::new()),
        }
    }

    fn put_intents(&mut self, id: NoteId, intents: &[Intent]) -> Result<()> {
        let raw = serde_json::to_vec(intents)
            .map_err(|e| CoreError::Storage(format!("unwritable access intents: {e}")))?;
        self.store.put_meta(id, ACCESS_INTENTS, &raw)
    }

    /// Keep an envelope this device cannot read yet.
    pub(super) fn retain(&mut self, id: NoteId, envelope: &[u8]) -> Result<()> {
        self.store.append(id, envelope)
    }

    /// A package's bytes, from the log, checked again.
    fn find_package(
        &self,
        id: NoteId,
        digest: &access::Digest,
        owner: AuthorKey,
    ) -> Result<Option<Package>> {
        for raw in self.store.load(id)? {
            if access::digest(&raw) == *digest {
                return Package::parse(&raw, id, owner).map(Some);
            }
        }
        Ok(None)
    }

    // -- recording what arrives ---------------------------------------------

    /// A package from the relay: checked against the owner key the record pins,
    /// and recorded.
    pub(super) fn receive_package(
        &mut self,
        id: NoteId,
        record: &NoteRecord,
        bytes: &[u8],
    ) -> Result<()> {
        let Some(owner) = record.owner() else {
            // A share-blob note has no owner to check a package against, and
            // so no packages.
            Envelope::decode(bytes)?.verify()?;
            return Ok(());
        };
        let package = Package::parse(bytes, id, owner)?;
        self.ingest(id, &package, true)?;
        Ok(())
    }

    /// Record one checked package: in the log, then in the ledger. Returns
    /// whether it was new.
    fn ingest(&mut self, id: NoteId, package: &Package, from_relay: bool) -> Result<bool> {
        let mut ledger = self.ledger(id)?;
        if let Some(entry) = ledger.get_mut(&package.digest) {
            if from_relay && !entry.on_relay {
                entry.on_relay = true;
                self.put_ledger(id, &ledger)?;
            }
            return Ok(false);
        }
        let granted = self.member().ok().and_then(|m| package.open_grant(&m));
        let readable = granted.clone().or_else(|| {
            self.keyring
                .note_key(id)
                .ok()
                .flatten()
                .filter(|key| key.id() == package.key_id())
        });
        let body = readable.and_then(|key| package.open_body(&key).ok());
        let mut entry = Entry::of(package, granted.is_some(), body.as_ref());
        entry.on_relay = from_relay;
        // The log first: a crash before the ledger is written leaves the
        // package where `ingest_logged_packages` finds it again.
        if !self.store.load(id)?.contains(&package.bytes) {
            self.store.append(id, &package.bytes)?;
        }
        ledger.insert(&package.digest, entry);
        self.put_ledger(id, &ledger)?;
        Ok(true)
    }

    /// Record every package in the log the ledger does not know: after a
    /// crash, or a store copied from elsewhere.
    pub(super) fn ingest_logged_packages(&mut self, id: NoteId) -> Result<()> {
        let Some(owner) = self.record(id).and_then(|record| record.owner()) else {
            return Ok(());
        };
        let ledger = self.ledger(id)?;
        let mut found = Vec::new();
        for raw in self.store.load(id)? {
            let Ok(env) = Envelope::decode(&raw) else {
                continue;
            };
            if env.kind != EnvelopeKind::Access || ledger.contains(&access::digest(&raw)) {
                continue;
            }
            if let Ok(package) = Package::parse(&raw, id, owner) {
                found.push(package);
            }
        }
        for package in found {
            self.ingest(id, &package, false)?;
        }
        Ok(())
    }

    /// Now that this device holds `key`, read the bodies of the packages sealed
    /// under it that it could not read when they arrived.
    fn refresh_entries(
        &self,
        id: NoteId,
        ledger: &mut Ledger,
        key: &NoteKey,
        owner: AuthorKey,
    ) -> Result<()> {
        let wanted: Vec<String> = ledger
            .packages
            .iter()
            .filter(|(_, e)| e.list.is_none() && e.key_id().is_ok_and(|k| k == key.id()))
            .map(|(d, _)| d.clone())
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        for raw in self.store.load(id)? {
            let digest = access::digest(&raw);
            let encoded = access::encode_digest(&digest);
            if !wanted.contains(&encoded) {
                continue;
            }
            let Ok(package) = Package::parse(&raw, id, owner) else {
                continue;
            };
            if let Ok(body) = package.open_body(key) {
                let entry = ledger.get_mut(&digest).expect("wanted is from the ledger");
                let fresh = Entry::of(&package, entry.mine, Some(&body));
                entry.snapshot = fresh.snapshot;
                entry.list = fresh.list;
                entry.added = fresh.added;
                entry.removed = fresh.removed;
            }
        }
        Ok(())
    }

    // -- acting on the current package --------------------------------------

    /// Rebuild an open note from its log, keeping whoever is watching it.
    fn reopen(&mut self, id: NoteId) -> Result<()> {
        let watch = self.open.remove(&id).map(|note| note.watch);
        self.replay(id)?;
        if let (Some(tx), Some(note)) = (watch, self.open.get_mut(&id)) {
            note.watch = tx;
            let _ = note.watch.send_replace(note.doc.view());
        }
        Ok(())
    }

    /// Move to the key the current package grants this person, if it is not
    /// the one this device holds (ADR 0023 §7).
    ///
    /// The document keeps everything it had: the snapshot the key was
    /// introduced with is merged into it, and whatever it holds that the new
    /// state lacks — this device's offline typing, edits it received just
    /// before — is sealed once under the new key and sent. The log becomes the
    /// introducing package, the updates already under the key, and that one
    /// update, so nothing under the old key is ever sent again.
    pub(super) fn switch_if_needed(&mut self, id: NoteId) -> Result<Option<Outgoing>> {
        let Some(owner) = self.record(id).and_then(|record| record.owner()) else {
            return Ok(None);
        };
        let Ok(member) = self.member() else {
            return Ok(None);
        };
        let mut ledger = self.ledger(id)?;
        let Some(current) = ledger.current() else {
            return Ok(None);
        };
        let Some(package) = self.find_package(id, &current, owner)? else {
            return Ok(None);
        };
        let Some(key) = package.open_grant(&member) else {
            return Ok(None);
        };
        let held = self.keyring.note_key(id)?;
        if held.as_ref().is_some_and(|held| held.id() == key.id()) {
            return Ok(None);
        }
        if held.is_some() && !self.open.contains_key(&id) {
            self.replay(id)?;
        }
        self.refresh_entries(id, &mut ledger, &key, owner)?;

        // The new state as the relay already has it: the snapshot the key came
        // with, and every update already sealed under it.
        let introduced = ledger.introduced(&current);
        let mut base = D::create(&self.actor);
        let mut keep = Vec::new();
        if let Some(digest) = introduced {
            if let Some(intro) = self.find_package(id, &digest, owner)? {
                if let Some(snapshot) = intro.open_body(&key)?.snapshot {
                    base.merge_snapshot(&snapshot)?;
                }
                keep.push(intro.bytes);
            }
        }
        if introduced != Some(current) {
            keep.push(package.bytes.clone());
        }
        let me = self.identity.public_key();
        let mut next_counter = self
            .store
            .get_meta(id, NEXT_COUNTER)?
            .and_then(|raw| raw.as_slice().try_into().ok().map(u64::from_be_bytes))
            .unwrap_or(0);
        if let Some(note) = self.open.get(&id) {
            next_counter = next_counter.max(note.next_counter);
        }
        let held_id = held.as_ref().map(NoteKey::id);
        for raw in self.store.load(id)? {
            let env = Envelope::decode(&raw)?;
            if env.author == me {
                next_counter = next_counter.max(env.counter + 1);
            }
            if env.kind != EnvelopeKind::Update {
                continue;
            }
            match env.key_id {
                Some(k) if k == key.id() => {
                    base.apply_remote(&env.open(&key)?)?;
                    keep.push(raw);
                }
                Some(k) if Some(k) != held_id && !ledger.has_held(&k) => keep.push(raw),
                _ => {}
            }
        }
        let base = base.snapshot();

        let mut out = Outgoing {
            rekey: true,
            ..Outgoing::default()
        };
        if let Some(note) = self.open.get_mut(&id) {
            note.doc.merge_snapshot(&base)?;
            if let Some(update) = note.doc.missing_from(&base)? {
                let envelope =
                    Envelope::seal(&self.identity, id, &key, next_counter, &update)?.encode();
                next_counter += 1;
                keep.push(envelope.clone());
                out.envelopes.push(envelope);
            }
        }

        // Durable in an order a crash can resume from: the ledger (which key is
        // history), the counter, the log, and the key last. A device that stops
        // between the log and the key reopens under the old key, finds nothing
        // readable, and switches again from what the log now says.
        if let Some(held) = &held {
            ledger.hold(&held.id());
        }
        self.put_ledger(id, &ledger)?;
        self.store
            .put_meta(id, NEXT_COUNTER, &next_counter.to_be_bytes())?;
        self.store.replace_log(id, &keep)?;
        self.keyring.put_note_key(id, &key)?;
        self.reopen(id)?;
        if held.is_none() {
            // A note that could not be read a moment ago now has a card.
            self.adopt_unplaced()?;
        }
        self.reindex(id)?;
        self.refresh_library();
        Ok(Some(out))
    }

    /// Delete this device's copy if the current package has dropped this
    /// person (ADR 0023 §7).
    ///
    /// Only a package that once granted them and a current one that no longer
    /// does counts: a package that has not added them *yet* — an adding
    /// package still on its way, or an owner device's sibling that won a race —
    /// is waiting, not removal. Missing data is never treated as removal.
    fn removed_if_needed(&mut self, id: NoteId) -> Result<bool> {
        let Some(record) = self.record(id) else {
            return Ok(false);
        };
        let NoteRecord::Guest { owner, invite } = record else {
            return Ok(false);
        };
        let Ok(member) = self.member() else {
            return Ok(false);
        };
        let ledger = self.ledger(id)?;
        let Some(current) = ledger.current() else {
            return Ok(false);
        };
        let Some(package) = self.find_package(id, &current, owner)? else {
            return Ok(false);
        };
        if package.open_grant(&member).is_some() {
            return Ok(false);
        }
        if !ledger.lineage(&current).iter().any(|(_, entry)| entry.mine) {
            return Ok(false);
        }
        let title = self.title_of(id);
        // The marker first: it is the record that keeps every device of this
        // person, and every one restored later, from fetching the note again.
        self.apply_account(AccountEdit::MarkRemoved { note: id, invite })?;
        self.forget_local(id)?;
        self.removals.push((id, title));
        Ok(true)
    }

    fn title_of(&self, id: NoteId) -> String {
        self.index
            .get(&id)
            .map(|entry| entry.title.clone())
            .or_else(|| self.open.get(&id).map(|note| note.doc.view().title))
            .unwrap_or_default()
    }

    /// Everything this device holds of a note it no longer has access to: its
    /// placement in the account, and its local copy. Not ADR 0022's permanent
    /// deletion, which would block a later invite.
    fn forget_local(&mut self, id: NoteId) -> Result<()> {
        let placement = self.placement(id);
        let reminders = self
            .placements
            .reminders_on(id)
            .into_iter()
            .map(|reminder| reminder.id)
            .collect();
        self.apply_account(AccountEdit::ForgetNote {
            note: id,
            tags: placement.tags.into_iter().collect(),
            legacy_tags: placement.legacy_tags.into_iter().collect(),
            reminders,
        })?;
        self.purge_note_local(id)
    }

    /// Drop the local copy of any note another device of this person was
    /// removed from.
    pub(super) fn reconcile_removed_notes(&mut self) -> Result<()> {
        let entries = self.account.doc.entries();
        let records = account::note_access_of(entries.clone());
        let stored: HashSet<NoteId> = self.store.notes()?.into_iter().collect();
        for (note, _) in account::removed_of(entries) {
            if records.contains_key(&note) {
                continue;
            }
            if stored.contains(&note) || self.keyring.note_key(note)?.is_some() {
                let title = self.title_of(note);
                self.purge_note_local(note)?;
                self.removals.push((note, title));
            }
        }
        Ok(())
    }

    /// The notes this device was just removed from, with their titles, for
    /// the message “You no longer have access to ‘Groceries’”. Each is handed
    /// out once.
    pub fn take_removals(&mut self) -> Vec<(NoteId, String)> {
        std::mem::take(&mut self.removals)
    }

    // -- the owner's side ---------------------------------------------------

    /// The owner's own entry on a list, as it stands now.
    fn owner_entry(&self, member: &MemberIdentity) -> Member {
        Member {
            member: member.id(),
            box_key: member.box_public(),
            owner: true,
            invite: OWNER_INVITE,
            name: self.profile_name().unwrap_or_default(),
        }
    }

    /// The list the current package names, or `None` before the first.
    fn current_list(&self, ledger: &Ledger) -> Result<Option<BTreeMap<Membership, Member>>> {
        let Some(current) = ledger.current() else {
            return Ok(None);
        };
        let members = ledger
            .get(&current)
            .map(Entry::members)
            .transpose()?
            .flatten()
            .unwrap_or_default();
        Ok(Some(
            members.into_iter().map(|m| (m.membership(), m)).collect(),
        ))
    }

    /// On an owner device, publish the package that makes the current list
    /// what the owner meant: every addition any package made, minus every
    /// removal, plus this device's own unfinished approvals and removals
    /// (ADR 0023 §5). Nothing when they already agree.
    fn build_if_needed(&mut self, id: NoteId) -> Result<Option<Outgoing>> {
        let Some(record) = self.record(id) else {
            return Ok(None);
        };
        let Some(owner_identity) = record.owner_identity() else {
            return Ok(None);
        };
        let Ok(member) = self.member() else {
            return Ok(None);
        };
        let intents = self.intents(id)?;
        let mut ledger = self.ledger(id)?;
        let current = ledger.current();
        if current.is_none() && !intents.iter().any(|i| matches!(i, Intent::Add { .. })) {
            return Ok(None);
        }
        // Up to date with the key this device holds before building on it.
        self.ensure_open(id)?;
        let Some(held) = self.keyring.note_key(id)? else {
            return Ok(None);
        };
        let current_list = self.current_list(&ledger)?.unwrap_or_default();

        let me = self.owner_entry(&member);
        let mut intended = if current.is_some() {
            ledger.intended()?
        } else {
            BTreeMap::new()
        };
        intended.retain(|_, m| !m.owner);
        intended.insert(me.membership(), me);
        for intent in &intents {
            let membership = intent.membership()?;
            match intent.member()? {
                Some(added) => {
                    intended.insert(membership, added);
                }
                None => {
                    intended.remove(&membership);
                }
            }
        }
        if intended.len() > MAX_MEMBERS {
            return Err(CoreError::Crypto("a note has at most 32 people"));
        }
        let now: BTreeSet<Membership> = current_list.keys().copied().collect();
        let meant: BTreeSet<Membership> = intended.keys().copied().collect();
        if current.is_some() && now == meant {
            return Ok(None);
        }
        let added: Vec<Membership> = meant.difference(&now).copied().collect();
        let removed: Vec<Membership> = now.difference(&meant).copied().collect();
        let rekey = !removed.is_empty();
        let (key, snapshot) = if rekey {
            let snapshot = self
                .open
                .get(&id)
                .ok_or_else(|| CoreError::UnknownNote(id.encode()))?
                .doc
                .snapshot();
            (NoteKey::random(), Some(snapshot))
        } else {
            (held.clone(), None)
        };
        let body = Body {
            list: intended.into_values().collect(),
            added,
            removed,
            snapshot,
        };
        let revision = match current {
            Some(digest) => ledger.get(&digest).map_or(0, |e| e.revision) + 1,
            None => 1,
        };
        let bytes = access::build(&owner_identity, id, revision, current, &key, &body)?;
        let package = Package::parse(&bytes, id, owner_identity.public_key())?;
        let entry = Entry::of(&package, true, Some(&body));

        // The package goes into the log before anything else is written, so a
        // crash anywhere below leaves it where the next launch finds it.
        self.store.append(id, &bytes)?;
        ledger.insert(&package.digest, entry);
        if !rekey {
            self.put_ledger(id, &ledger)?;
            return Ok(Some(Outgoing {
                rekey: false,
                envelopes: vec![bytes],
                removed: false,
            }));
        }
        // A removal: the package is now the whole of the note. Everything this
        // device held under the old key is in its snapshot, and none of it may
        // be sent under that key again (ADR 0023 §6).
        ledger.hold(&held.id());
        self.put_ledger(id, &ledger)?;
        self.store.replace_log(id, std::slice::from_ref(&bytes))?;
        self.keyring.put_note_key(id, &key)?;
        self.reopen(id)?;
        self.reindex(id)?;
        self.refresh_library();
        Ok(Some(Outgoing {
            rekey: true,
            envelopes: vec![bytes],
            removed: false,
        }))
    }

    /// Change the note's key now, with the same people on it
    /// ([ADR 0023](../../../docs/adr/0023-access-packages.md) §6, used by
    /// [ADR 0030](../../../docs/adr/0030-a-link-lets-its-first-person-in.md)).
    ///
    /// What a decline does when the invite had a pre-grant: whoever held the
    /// link may have read the note already, and rotating is the only thing that
    /// stops them reading it any further. Everybody else re-keys on the
    /// package, exactly as after a removal.
    pub fn rotate_key(&mut self, id: NoteId) -> Result<Option<Outgoing>> {
        let Some(owner_identity) = self.record(id).and_then(|record| record.owner_identity())
        else {
            return Ok(None);
        };
        self.ensure_open(id)?;
        let mut ledger = self.ledger(id)?;
        let Some(held) = self.keyring.note_key(id)? else {
            return Ok(None);
        };
        let current = ledger.current();
        let list: Vec<Member> = match current {
            Some(_) => self
                .current_list(&ledger)?
                .unwrap_or_default()
                .into_values()
                .collect(),
            // A note that was never shared has no package and no list; the
            // first one this makes has the owner on it and nobody else.
            None => {
                let member = self.member()?;
                vec![self.owner_entry(&member)]
            }
        };
        let revision = match current {
            Some(digest) => ledger.get(&digest).map_or(0, |e| e.revision) + 1,
            None => 1,
        };
        let key = NoteKey::random();
        let snapshot = self
            .open
            .get(&id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?
            .doc
            .snapshot();
        let body = Body {
            list,
            added: Vec::new(),
            removed: Vec::new(),
            snapshot: Some(snapshot),
        };
        let bytes = access::build(&owner_identity, id, revision, current, &key, &body)?;
        let package = Package::parse(&bytes, id, owner_identity.public_key())?;
        let entry = Entry::of(&package, true, Some(&body));

        // Durable in the order a crash resumes from: log, ledger, counter,
        // replacement log, key last (ADR 0023 §7).
        self.store.append(id, &bytes)?;
        ledger.insert(&package.digest, entry);
        ledger.hold(&held.id());
        self.put_ledger(id, &ledger)?;
        self.store.replace_log(id, std::slice::from_ref(&bytes))?;
        self.keyring.put_note_key(id, &key)?;
        self.reopen(id)?;
        self.reindex(id)?;
        self.refresh_library();
        Ok(Some(Outgoing {
            rekey: true,
            envelopes: vec![bytes],
            removed: false,
        }))
    }

    /// Forget the intents the current package satisfies, once the relay has it.
    fn settle_intents(&mut self, id: NoteId) -> Result<()> {
        let intents = self.intents(id)?;
        if intents.is_empty() {
            return Ok(());
        }
        let ledger = self.ledger(id)?;
        let Some(current) = ledger.current() else {
            return Ok(());
        };
        if !ledger.get(&current).is_some_and(|e| e.on_relay) {
            return Ok(());
        }
        let listed: BTreeSet<Membership> = self
            .current_list(&ledger)?
            .unwrap_or_default()
            .into_keys()
            .collect();
        let mut kept = Vec::new();
        for intent in intents {
            let membership = intent.membership()?;
            let done = match intent {
                Intent::Add { .. } => listed.contains(&membership),
                Intent::Remove { .. } => !listed.contains(&membership),
            };
            if !done {
                kept.push(intent);
            }
        }
        self.put_intents(id, &kept)
    }

    /// Whether this device is waiting to remove somebody from a note. While it
    /// is, nothing but packages is sent for the note: an update now would be
    /// under the key the person being removed holds (ADR 0023 §6).
    pub fn holds_back(&self, id: NoteId) -> Result<bool> {
        Ok(self
            .intents(id)?
            .iter()
            .any(|intent| matches!(intent, Intent::Remove { .. })))
    }

    /// Packages this device made that the relay has not echoed back — or, on
    /// an owner device starting from nothing, every package it has — as
    /// `(revision, envelope)`.
    pub(super) fn unsent_packages(&self, id: NoteId, all: bool) -> Result<Vec<(u64, Vec<u8>)>> {
        let ledger = self.ledger(id)?;
        let owned = self.record(id).is_some_and(|record| record.owned());
        let wanted: BTreeSet<String> = ledger
            .packages
            .iter()
            .filter(|(_, entry)| !entry.on_relay || (all && owned))
            .map(|(digest, _)| digest.clone())
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for raw in self.store.load(id)? {
            let Ok(env) = Envelope::decode(&raw) else {
                continue;
            };
            if env.kind == EnvelopeKind::Access
                && wanted.contains(&access::encode_digest(&access::digest(&raw)))
            {
                out.push((env.counter, raw));
            }
        }
        out.sort_by_key(|(revision, _)| *revision);
        Ok(out)
    }

    /// The note has settled: act on its packages, and say what to send.
    ///
    /// Called by the sync engine once the relay's backlog for the note is
    /// drained, and for every arrival after that — and by the app when the
    /// owner asks for a change, so that a device that is already caught up
    /// does not wait for the next arrival to make it.
    pub fn settle_note(&mut self, id: NoteId) -> Result<Outgoing> {
        if self.is_account(id)
            || account::is_deleted(self.account.doc.entries(), id)
            || self.record(id).is_none()
        {
            return Ok(Outgoing::default());
        }
        let holding = self.holds_back(id)?;
        let mut out = self.switched.remove(&id).unwrap_or_default();
        if let Some(switched) = self.switch_if_needed(id)? {
            out.then(switched);
        }
        if self.removed_if_needed(id)? {
            return Ok(Outgoing {
                removed: true,
                ..Outgoing::default()
            });
        }
        if let Some(built) = self.build_if_needed(id)? {
            // A new key's package holds everything a switch's update did,
            // under a key the update was not sealed with.
            out.then(built);
        }
        self.settle_intents(id)?;
        // A package that only adds people changes no key and so no document,
        // but it changes who a search can find the note by.
        if self.open.contains_key(&id)
            && self.index.get(&id).map(|entry| &entry.collaborators)
                != Some(&self.collaborators(id)?)
        {
            self.reindex(id)?;
            self.refresh_library();
        }
        if holding && !self.holds_back(id)? {
            // What was typed while the removal waited is under the new key now
            // and was never sent.
            let sent = self.sync_cursor(id)?.sent_upto;
            let me = self.identity.public_key();
            for raw in self.store.load(id)? {
                let env = Envelope::decode(&raw)?;
                if env.author == me
                    && env.kind == EnvelopeKind::Update
                    && sent.is_none_or(|sent| env.counter > sent)
                    && !out.envelopes.contains(&raw)
                {
                    out.envelopes.push(raw);
                }
            }
        }
        Ok(out)
    }

    // -- what the screens ask -----------------------------------------------

    /// Everybody with access to a note, owner first.
    pub fn people(&self, id: NoteId) -> Result<People> {
        let record = self
            .record(id)
            .ok_or_else(|| CoreError::UnknownNote(id.encode()))?;
        let can_manage = record.owned();
        let me = self.member().ok().map(|m| m.id());
        let ledger = self.ledger(id)?;
        let intents = self.intents(id)?;
        let removing: BTreeSet<Membership> = intents
            .iter()
            .filter(|i| matches!(i, Intent::Remove { .. }))
            .filter_map(|i| i.membership().ok())
            .collect();
        let list = self.current_list(&ledger)?;
        let shared = list.as_ref().is_some_and(|l| l.values().any(|m| !m.owner));
        let mut people: Vec<Person> = match list {
            Some(list) => list.into_values().collect(),
            None => match (&record, self.member()) {
                (NoteRecord::Owner { .. }, Ok(member)) => vec![self.owner_entry(&member)],
                _ => Vec::new(),
            },
        }
        .into_iter()
        .map(|m| Person {
            member: m.member.encode(),
            invite: if m.owner {
                String::new()
            } else {
                m.invite.encode()
            },
            removing: removing.contains(&m.membership()),
            me: Some(m.member) == me,
            owner: m.owner,
            name: m.name,
        })
        .collect();
        people.sort_by(|a, b| {
            b.owner
                .cmp(&a.owner)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.member.cmp(&b.member))
        });
        let listed: BTreeSet<Membership> = self
            .current_list(&ledger)?
            .unwrap_or_default()
            .into_keys()
            .collect();
        let adding = intents
            .iter()
            .filter_map(|i| i.member().ok().flatten())
            .filter(|m| !listed.contains(&m.membership()))
            .map(|m| m.name)
            .collect();
        Ok(People {
            shared,
            can_manage,
            people,
            adding,
        })
    }

    /// The people a search can filter by: everybody on the current list but
    /// this person, one entry per member ID.
    pub fn collaborators(&self, id: NoteId) -> Result<Vec<crate::collaborators::Collaborator>> {
        let me = self.member().ok().map(|m| m.id());
        let ledger = self.ledger(id)?;
        let mut people: Vec<_> = self
            .current_list(&ledger)?
            .unwrap_or_default()
            .into_values()
            .filter(|m| Some(m.member) != me)
            .map(|m| crate::collaborators::Collaborator {
                id: m.member.encode(),
                name: m.name,
            })
            .collect();
        people.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.name.cmp(&b.name)));
        people.dedup_by(|a, b| a.id == b.id);
        people.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(people)
    }

    /// The digest of the note's current access package, if it has one.
    pub fn current_package(&self, id: NoteId) -> Result<Option<access::Digest>> {
        Ok(self.ledger(id)?.current())
    }

    pub fn can_manage_collaborators(&self, id: NoteId) -> bool {
        self.record(id).is_some_and(|record| record.owned())
    }

    /// Whether this device can open a note: it has been granted a key.
    pub fn can_open(&self, id: NoteId) -> bool {
        !self.is_account(id)
            && !account::is_deleted(self.account.doc.entries(), id)
            && self.keyring.note_key(id).ok().flatten().is_some()
    }

    /// Ask to remove one person. Saved at once, made when this device is
    /// caught up with the note; the row says “Removing…” until then.
    pub fn remove_person(&mut self, id: NoteId, member: &str, invite: &str) -> Result<()> {
        if !self.can_manage_collaborators(id) {
            return Err(CoreError::Crypto("only the owner can remove people"));
        }
        let membership = Membership {
            member: AuthorKey(decode_32(member)?),
            invite: InviteId::decode(invite)?,
        };
        if membership.invite == OWNER_INVITE {
            return Err(CoreError::Crypto("the owner cannot be removed"));
        }
        let mut intents = self.intents(id)?;
        intents.retain(|i| i.membership().ok() != Some(membership));
        intents.push(Intent::remove(&membership));
        self.put_intents(id, &intents)
    }

    /// Remove everyone but the owner. The app also withdraws the note's open
    /// invites, which only it can reach the relay to do.
    pub fn stop_sharing(&mut self, id: NoteId) -> Result<()> {
        if !self.can_manage_collaborators(id) {
            return Err(CoreError::Crypto("only the owner can stop sharing"));
        }
        let ledger = self.ledger(id)?;
        let mut intents: Vec<Intent> = self
            .intents(id)?
            .into_iter()
            .filter(|i| matches!(i, Intent::Remove { .. }))
            .collect();
        for (membership, member) in self.current_list(&ledger)?.unwrap_or_default() {
            if member.owner
                || intents
                    .iter()
                    .any(|i| i.membership().ok() == Some(membership))
            {
                continue;
            }
            intents.push(Intent::remove(&membership));
        }
        self.put_intents(id, &intents)?;
        for invite in self.invites()? {
            if invite.note == id && invite.approval.is_some() {
                // An approval not yet answered would let somebody in after
                // sharing stopped.
                let mut kept = invite.clone();
                kept.approval = None;
                self.remember_invite(&kept)?;
            }
        }
        Ok(())
    }

    /// The owner approved `request` on `invite`: save it, before anything
    /// else happens (ADR 0023 §3, step 1).
    pub fn approve(
        &mut self,
        invite: crate::ids::InviteId,
        request: &str,
        join: &crate::invite::JoinRequest,
    ) -> Result<Approved> {
        let mut outgoing = self
            .invites()?
            .into_iter()
            .find(|i| i.secret.id() == invite)
            .ok_or_else(|| CoreError::Invite("This device has no such invite open.".into()))?;
        let note = outgoing.note;
        if !self.can_manage_collaborators(note) {
            return Err(CoreError::Crypto("only the owner can let people in"));
        }
        let ledger = self.ledger(note)?;
        let listed = self.current_list(&ledger)?.unwrap_or_default();
        outgoing.approval = Some((request.to_string(), join.clone()));
        self.remember_invite(&outgoing)?;
        if listed.values().any(|m| m.member == join.member) {
            return Ok(Approved::AlreadyHasAccess);
        }
        let member = Member {
            member: join.member,
            box_key: join.box_key,
            owner: false,
            invite,
            name: access::clip_name(&join.name).to_string(),
        };
        let mut intents = self.intents(note)?;
        let pending = intents
            .iter()
            .filter(|i| matches!(i, Intent::Add { .. }))
            .count();
        if listed.len().max(1) + pending + 1 > MAX_MEMBERS {
            return Err(CoreError::Invite(
                "A note can be shared with 32 people at most.".into(),
            ));
        }
        intents.retain(|i| i.membership().ok() != Some(member.membership()));
        intents.push(Intent::add(&member));
        self.put_intents(note, &intents)?;
        Ok(Approved::Adding)
    }

    /// The sealed answer to an approved request, once the relay has the
    /// package that lets that person in (ADR 0023 §3, step 3). `None` while
    /// it is still on its way.
    pub fn approval_answer(&self, invite: crate::ids::InviteId) -> Result<Option<Vec<u8>>> {
        let outgoing = self
            .invites()?
            .into_iter()
            .find(|i| i.secret.id() == invite)
            .ok_or_else(|| CoreError::Invite("This device has no such invite open.".into()))?;
        let Some((_, join)) = &outgoing.approval else {
            return Err(CoreError::Invite(
                "That request has not been approved.".into(),
            ));
        };
        let note = outgoing.note;
        let owner = self
            .record(note)
            .and_then(|record| record.owner())
            .ok_or_else(|| CoreError::UnknownNote(note.encode()))?;
        let ledger = self.ledger(note)?;
        let Some(current) = ledger.current() else {
            return Ok(None);
        };
        if !ledger.get(&current).is_some_and(|e| e.on_relay) {
            return Ok(None);
        }
        let listed = self.current_list(&ledger)?.unwrap_or_default();
        if !listed.values().any(|m| m.member == join.member) {
            return Ok(None);
        }
        crate::invite::seal_answer(&outgoing.secret, join, note, owner).map(Some)
    }

    /// The sealed pre-grant for an open invite: the note key its link hands to
    /// whoever claims it ([ADR 0030](../../../docs/adr/0030-a-link-lets-its-first-person-in.md)).
    ///
    /// `None` when this device is not the note's owner, or holds no key for it,
    /// which is what an invite made before the note was readable looks like.
    pub fn pre_grant(&self, invite: crate::ids::InviteId) -> Result<Option<Vec<u8>>> {
        let Some(outgoing) = self
            .invites()?
            .into_iter()
            .find(|i| i.secret.id() == invite)
        else {
            return Ok(None);
        };
        let note = outgoing.note;
        if !self.can_manage_collaborators(note) {
            return Ok(None);
        }
        let Some(owner) = self.record(note).and_then(|record| record.owner()) else {
            return Ok(None);
        };
        let Some(key) = self.keyring.note_key(note)? else {
            return Ok(None);
        };
        crate::invite::seal_pre_grant(&outgoing.secret, note, owner, &key).map(Some)
    }

    /// A pre-grant opened: a place for the note and the key to read it with,
    /// before the owner's device has added this person.
    ///
    /// The key is *provisional*. It is written to the keyring like a granted
    /// one, because that is what makes the note readable and syncable, and
    /// ADR 0023 §7 already does the right thing with it: a package that has not
    /// added this person yet is waiting rather than a removal, and the package
    /// that does add them either grants this same key or rotates it and the
    /// ordinary switch follows.
    ///
    /// An existing key is never replaced by one read from a link: it may be
    /// newer, and a link is not authority over a note this device already holds.
    pub fn accept_pre_grant(
        &mut self,
        invite: crate::ids::InviteId,
        note: NoteId,
        owner: AuthorKey,
        key: crate::crypto::NoteKey,
    ) -> Result<NoteId> {
        self.accept_answer(invite, note, owner)?;
        if self.record(note).is_some_and(|record| record.owned()) {
            return Ok(note);
        }
        if self.keyring.note_key(note)?.is_none() {
            self.keyring.put_note_key(note, &key)?;
            self.refresh_library();
        }
        Ok(note)
    }

    /// Give up the notes this device was reading only through an invite that
    /// was turned away. Returns what went, with its title, for a message.
    ///
    /// Only notes whose record is this invite are dropped: a person let in and
    /// later removed is a different story, and carries its own marker.
    pub fn forget_pre_grant(
        &mut self,
        invite: crate::ids::InviteId,
    ) -> Result<Vec<(NoteId, String)>> {
        let records = account::note_access_of(self.account.doc.entries());
        let mut gone = Vec::new();
        for (note, record) in records {
            if let NoteRecord::Guest { invite: held, .. } = record {
                if held != invite {
                    continue;
                }
                let title = self.title_of(note);
                self.forget_local(note)?;
                gone.push((note, title));
            }
        }
        Ok(gone)
    }

    /// An answer opened: record the note in this person's account and give it
    /// a place. The key arrives with the note's current package.
    pub fn accept_answer(
        &mut self,
        invite: crate::ids::InviteId,
        note: NoteId,
        owner: AuthorKey,
    ) -> Result<NoteId> {
        if self.is_account(note) {
            return Err(CoreError::MalformedInvite(
                "the account document is not a note",
            ));
        }
        if account::is_deleted(self.account.doc.entries(), note) {
            return Err(CoreError::MalformedInvite(
                "this note was permanently deleted",
            ));
        }
        if account::removed_of(self.account.doc.entries()).contains(&(note, invite)) {
            return Err(CoreError::Invite(
                "You no longer have access to this note.".into(),
            ));
        }
        match self.record(note) {
            Some(NoteRecord::Owner { .. }) => return Ok(note),
            Some(NoteRecord::Guest { invite: held, .. }) if held == invite => {}
            _ => {
                self.apply_account(AccountEdit::SetNoteRecord {
                    note,
                    record: NoteRecord::Guest { owner, invite }.encode(),
                })?;
            }
        }
        if self.ordinal(note)? == 0 {
            let next = self.next_ordinal()?;
            self.store.put_meta(note, ADDED, &next.to_be_bytes())?;
        }
        if !self.placements.placements.contains_key(&note) {
            self.place(note, 0)?;
        }
        Ok(note)
    }
}
