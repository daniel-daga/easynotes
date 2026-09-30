//! Moving an attachment's sealed bytes to and from the relay
//! ([ADR 0027](../../../docs/adr/0027-attachments.md) §4).
//!
//! Written once over the transport's `call`, as the invite calls are over its
//! `post`, and once over [`MediaStorage`], so a tab moves bytes between the
//! relay and IndexedDB with the same code a device uses for its directory.
//!
//! Both directions go a chunk at a time and hold one chunk in memory. Neither
//! decrypts anything: an upload sends what the media store holds, and a
//! download is checked against the descriptor's length and digest before it is
//! kept. So a relay, or anyone between here and it, sees only ciphertext going
//! in and can only get ciphertext that is exactly the attached file accepted
//! coming out.

use super::transport::call;
use super::wire;
use crate::attachment::storage::{Fut, IncomingMedia, MediaStorage};
use crate::attachment::{Media, MediaRef};
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// What a call carries. The transport turns it into a body or a header.
pub(crate) enum Request {
    Empty,
    Json(String),
    Bytes(Vec<u8>),
    /// `Range: bytes=at-(at + len - 1)`.
    Range(u64, u64),
}

#[derive(Serialize)]
struct Reserve {
    sealed_len: u64,
    chunk_size: u32,
}

#[derive(Deserialize)]
struct Status {
    committed: bool,
    received: Vec<u32>,
}

#[derive(Serialize)]
struct Commit {
    digest: String,
}

fn url(http_base: &str, note: NoteId, media: &MediaRef, rest: &str) -> String {
    wire::api(
        http_base,
        &format!("/media/{}/{}{rest}", note.encode(), media.object.encode()),
    )
}

/// What a status the caller did not expect means, in words a person can be
/// shown where there are any.
fn refused(status: u16, body: &[u8]) -> CoreError {
    if let Some(outdated) = wire::refused(status, body) {
        return outdated;
    }
    match status {
        401 => CoreError::Storage("relay: the device token was refused".into()),
        404 => CoreError::Attachment("the relay does not have this attachment".into()),
        410 => CoreError::Attachment("this note was deleted from the relay".into()),
        503 => CoreError::Attachment("this relay does not take attachments".into()),
        507 => CoreError::Attachment("the relay has no room for this attachment".into()),
        other => CoreError::Storage(format!("relay: media call answered {other}")),
    }
}

/// Where chunk `index` of a sealed variant starts, and how long it is.
fn part(media: &MediaRef, index: u32) -> (u64, u64) {
    let full = media.chunk_size as u64 + crate::attachment::seal::CHUNK_OVERHEAD as u64;
    let at = index as u64 * full;
    (at, full.min(media.sealed_len() - at))
}

/// Put one sealed variant on the relay and commit it, from wherever it got to.
///
/// `chunk` hands back sealed chunk `index` as the media store holds it — a
/// future, because in a tab the store has to be waited for. Safe to
/// call again after any interruption, including after it succeeded: it asks
/// the relay what arrived and sends only the rest, and a commit that is
/// already done is done. If the relay finds the chunks are not the digest —
/// a torn write somewhere — it drops them, and this sends them all once more.
pub async fn upload<'c>(
    http_base: &str,
    token: &str,
    note: NoteId,
    media: &MediaRef,
    mut chunk: impl FnMut(u32) -> Fut<'c, Result<Vec<u8>>>,
) -> Result<()> {
    let reserve = serde_json::to_string(&Reserve {
        sealed_len: media.sealed_len(),
        chunk_size: media.chunk_size,
    })
    .expect("a reservation serialises");
    let (status, body) = call(
        "POST",
        &url(http_base, note, media, ""),
        token,
        Request::Json(reserve),
    )
    .await?;
    match status {
        200 | 201 => {}
        409 => {
            return Err(CoreError::Attachment(
                "the relay holds another file under this attachment's name".into(),
            ))
        }
        other => return Err(refused(other, &body)),
    }
    let commit = serde_json::to_string(&Commit {
        digest: URL_SAFE_NO_PAD.encode(media.digest),
    })
    .expect("a commit serialises");

    for _attempt in 0..2 {
        let (status, body) = call(
            "GET",
            &url(http_base, note, media, "/status"),
            token,
            Request::Empty,
        )
        .await?;
        if status != 200 {
            return Err(refused(status, &body));
        }
        let now: Status = serde_json::from_slice(&body)
            .map_err(|e| CoreError::Storage(format!("relay: an unreadable media status: {e}")))?;
        if now.committed {
            // Committed by an earlier run that did not hear the answer, or by
            // this one. Committing again checks it is under our digest.
        } else {
            for index in (0..media.chunks()).filter(|i| !now.received.contains(i)) {
                let (status, body) = call(
                    "PUT",
                    &url(http_base, note, media, &format!("/{index}")),
                    token,
                    Request::Bytes(chunk(index).await?),
                )
                .await?;
                if !(200..300).contains(&status) {
                    return Err(refused(status, &body));
                }
            }
        }
        let (status, body) = call(
            "POST",
            &url(http_base, note, media, "/commit"),
            token,
            Request::Json(commit.clone()),
        )
        .await?;
        match status {
            200 => return Ok(()),
            // Missing chunks, or chunks that were not the digest and have been
            // dropped: ask again and send what is needed, once.
            409 => continue,
            other => return Err(refused(other, &body)),
        }
    }
    Err(CoreError::Attachment(
        "the relay would not take this attachment whole".into(),
    ))
}

/// Fetch one sealed variant a chunk at a time, in order, into `incoming`,
/// calling `each` after every chunk.
///
/// `incoming` is where the bytes go to be checked: it keeps nothing whose
/// length and digest are not the descriptor's. This checks each chunk's length,
/// so a relay cannot make it read more than the descriptor says. It does not
/// finish `incoming`; the caller does, once this has returned.
pub async fn download<I: IncomingMedia>(
    http_base: &str,
    token: &str,
    note: NoteId,
    media: &MediaRef,
    incoming: &mut I,
    mut each: impl FnMut(),
) -> Result<()> {
    for index in 0..media.chunks() {
        let (at, len) = part(media, index);
        let (status, body) = call(
            "GET",
            &url(http_base, note, media, ""),
            token,
            Request::Range(at, len),
        )
        .await?;
        match status {
            206 if body.len() as u64 == len => incoming.write(&body).await?,
            206 | 200 => return Err(CoreError::Crypto("an attachment chunk of the wrong length")),
            other => return Err(refused(other, &body)),
        }
        each();
    }
    Ok(())
}

/// Upload an attachment's original and thumbnail from this device's media
/// store. What a staged import does before its descriptor is published.
pub async fn upload_media<S: MediaStorage>(
    http_base: &str,
    token: &str,
    store: &S,
    note: NoteId,
    media: &Media,
) -> Result<()> {
    let variants =
        std::iter::once(&media.original).chain(media.thumbnail.as_ref().map(|t| &t.media));
    for variant in variants {
        upload(http_base, token, note, variant, |index| {
            store.read_sealed_chunk(note, variant, index)
        })
        .await?;
    }
    Ok(())
}

/// Download one variant into this device's media store, unless it is already
/// there. Kept only once it has proved to be the file attached.
pub async fn download_into<S: MediaStorage>(
    http_base: &str,
    token: &str,
    store: &S,
    note: NoteId,
    media: &MediaRef,
) -> Result<()> {
    if store.has(note, media).await {
        return Ok(());
    }
    let mut incoming = store.incoming(note, media).await?;
    download(http_base, token, note, media, &mut incoming, || {}).await?;
    incoming.finish().await
}
