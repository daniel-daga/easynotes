//! How much one note may keep, and who may shrink it.
//!
//! [ADR 0024](../../docs/decisions.md#adr-0024). The relay's log for a
//! note used to only grow. Now a member that is caught up can hand the relay a
//! **base** — one sealed update holding the whole document — with a promise
//! that it covers everything up to a sequence number. The relay drops what that
//! makes redundant, and caps what remains.
//!
//! Everything here is a pure function of a topic's bookkeeping and the bytes
//! in front of it, so that the memory store and Postgres cannot disagree about
//! any of it: each reads the topic, asks here, and writes what it is told.
//!
//! What the relay reads to do this, in full:
//!   * an envelope's kind, author and signature — only for access packages,
//!     whose signed prefix names who may compact;
//!   * a compaction request's signature, under the key that prefix names.
//!
//! It still decrypts nothing, and still learns nothing about who anybody is:
//! the owner key and the compaction key are random per note, and it saw the
//! owner key already as the author of every package.

use easynotes_core::access::{Digest, Package};
use easynotes_core::compaction::Request;
use easynotes_core::crypto::{AuthorKey, Envelope, EnvelopeKind, SIG_LEN};
use easynotes_core::ids::{MediaObjectId, NoteId};
use easynotes_core::purge;

/// What a deployment allows. The defaults are ADR 0024's starting values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// A note whose live log holds more than this many bytes refuses appends
    /// with `note_full`. Set far above what compaction leaves, so only a broken
    /// client or abuse reaches it.
    pub note_bytes: u64,
    /// Past this many compactable envelopes, an appender is asked to compact.
    pub crowded_envelopes: u64,
    /// Or past this many compactable bytes.
    pub crowded_bytes: u64,
    /// Once every note together holds this much, the relay refuses to start
    /// new notes (`relay_full`). Appends to notes it already has still go:
    /// a flood harms people creating notes, not people using them.
    pub ceiling: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            note_bytes: 32 << 20,
            crowded_envelopes: 1024,
            crowded_bytes: 4 << 20,
            ceiling: None,
        }
    }
}

/// The current access package, as far as the relay needs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Current {
    pub revision: u64,
    pub digest: Digest,
    /// The compaction key the package names.
    pub compaction: [u8; 32],
}

/// One topic's bookkeeping. What both stores keep beside the log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Topic {
    pub head: i64,
    /// Every envelope stored, and their bytes: what the cap applies to.
    pub live_envelopes: u64,
    pub live_bytes: u64,
    /// The envelopes a compaction could drop, and their bytes: what "crowded"
    /// is judged on. Kept packages are live and not loose.
    pub loose_envelopes: u64,
    pub loose_bytes: u64,
    /// The note's owner key, pinned by the first package that verifies.
    pub owner: Option<[u8; 32]>,
    pub current: Option<Current>,
    /// The compaction key the first compaction brought, for a topic with no
    /// package: a note never shared, or the account document.
    pub registered: Option<[u8; 32]>,
    /// How far the newest base reaches; 0 before the first.
    pub covers: i64,
    /// Purged ([ADR 0026](../../docs/decisions.md#adr-0026)): nothing
    /// is stored and nothing will be. Kept so a device still holding the topic
    /// cannot bring it back.
    pub gone: bool,
}

impl Topic {
    /// Whether an appender should be asked to compact.
    pub fn crowded(&self, limits: &Limits) -> bool {
        self.loose_envelopes > limits.crowded_envelopes || self.loose_bytes > limits.crowded_bytes
    }

    /// Who may compact now.
    pub fn authority(&self) -> Option<[u8; 32]> {
        self.current.map(|c| c.compaction).or(self.registered)
    }
}

/// What to do with an append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classified {
    /// Whether compaction must leave it: an access package signed by the
    /// owner. Every device needs the whole chain of those to know who has
    /// access, so they are never compacted away.
    pub keep: bool,
    /// The owner key to pin, if this is the first package.
    pub owner: Option<[u8; 32]>,
    /// The package that is current once this is stored, if it changes.
    pub current: Option<Current>,
}

/// Classify an append. Anything that is not an owner-signed package is loose;
/// that includes bytes that are not an envelope at all, which only a test hands
/// the store directly.
pub fn classify(topic: &Topic, note: &str, raw: &[u8]) -> Classified {
    let loose = Classified {
        keep: false,
        owner: None,
        current: None,
    };
    let Ok(note_id) = NoteId::decode(note) else {
        return loose;
    };
    let Ok(envelope) = Envelope::decode(raw) else {
        return loose;
    };
    if envelope.kind != EnvelopeKind::Access {
        return loose;
    }
    // The first package pins the owner. A package is published only by an
    // owner device, and only once the note is shared, before anyone else has
    // its ID; a stranger cannot get in first on a topic nobody has told them.
    let owner = topic.owner.unwrap_or(envelope.author.0);
    let Ok(package) = Package::parse(raw, note_id, AuthorKey(owner)) else {
        return loose;
    };
    let newer = topic.current.is_none_or(|current| {
        package.revision > current.revision
            || (package.revision == current.revision && package.digest < current.digest)
    });
    Classified {
        keep: true,
        owner: topic.owner.is_none().then_some(owner),
        current: newer.then_some(Current {
            revision: package.revision,
            digest: package.digest,
            compaction: package.compaction.0,
        }),
    }
}

/// A compaction as it comes off the wire, before anything is checked.
pub struct Proposal<'a> {
    pub covers: i64,
    pub base: &'a [u8],
    /// The objects the base names: what the topic keeps of its media, the
    /// rest being doomed (ADR 0027). `None` keeps everything.
    pub media: Option<&'a [MediaObjectId]>,
    pub key: [u8; 32],
    pub signature: [u8; SIG_LEN],
}

/// Why a compaction was refused. Each is said to the device that asked, which
/// stops asking until it reconnects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not this topic's compaction key.
    Unauthorised,
    /// The signature does not verify.
    BadSignature,
    /// Reaches no further than the base the relay already has, or past the head.
    Stale,
    /// Not a sealed update for this note.
    NotABase,
    /// The topic was purged.
    Gone,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Unauthorised => "not this note's compaction key",
            Refusal::BadSignature => "the signature does not verify",
            Refusal::Stale => "stale",
            Refusal::NotABase => "not a base for this note",
            Refusal::Gone => crate::protocol::GONE,
        }
    }
}

/// Check a compaction. `Ok(register)` says whether to register its key, which
/// happens for the first compaction of a topic with no package.
pub fn authorise(topic: &Topic, note: &str, proposal: &Proposal<'_>) -> Result<bool, Refusal> {
    if topic.gone {
        return Err(Refusal::Gone);
    }
    let note_id = NoteId::decode(note).map_err(|_| Refusal::NotABase)?;
    let register = match topic.authority() {
        Some(key) if key == proposal.key => false,
        Some(_) => return Err(Refusal::Unauthorised),
        None => true,
    };
    let request = Request {
        covers: proposal.covers,
        base: proposal.base.to_vec(),
        media: proposal.media.map(<[MediaObjectId]>::to_vec),
        key: AuthorKey(proposal.key),
        signature: proposal.signature,
    };
    if !request.verifies(note_id) {
        return Err(Refusal::BadSignature);
    }
    if proposal.covers <= topic.covers || proposal.covers > topic.head {
        return Err(Refusal::Stale);
    }
    // An update is the only thing a base can be. A device refuses a snapshot
    // kind from the relay, and a package is not the relay's to make.
    let envelope = Envelope::decode(proposal.base).map_err(|_| Refusal::NotABase)?;
    if envelope.kind != EnvelopeKind::Update || envelope.note_id != note_id {
        return Err(Refusal::NotABase);
    }
    Ok(register)
}

/// Check a purge ([ADR 0026](../../docs/decisions.md#adr-0026)).
///
/// The authority is the one compaction already has, except that a shared
/// note's owner key outranks the compaction key its package names: every guest
/// holds the note key and so the compaction key, and a guest must not be able
/// to delete the owner's list for everyone. A topic with neither is trusted on
/// first use, as an unshared topic's compaction is. Purging what is already
/// gone is not checked at all: it succeeds, so an interrupted deletion can be
/// run again.
pub fn authorise_purge(
    topic: &Topic,
    note: &str,
    key: [u8; 32],
    signature: [u8; SIG_LEN],
) -> Result<(), Refusal> {
    if topic.gone {
        return Ok(());
    }
    let note_id = NoteId::decode(note).map_err(|_| Refusal::Unauthorised)?;
    let allowed = match (topic.owner, topic.registered) {
        (Some(owner), _) => key == owner,
        (None, Some(registered)) => key == registered,
        (None, None) => true,
    };
    if !allowed {
        return Err(Refusal::Unauthorised);
    }
    let request = purge::Request {
        key: AuthorKey(key),
        signature,
    };
    if !request.verifies(note_id) {
        return Err(Refusal::BadSignature);
    }
    Ok(())
}
