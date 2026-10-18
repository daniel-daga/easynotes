//! Ephemeral mailbox for a one-time, owner-approved account transfer.
//! The relay stores only random capabilities, a temporary public key and
//! ciphertext; sessions expire after five minutes or a relay restart.

use crate::{
    app::AppState,
    error::{RelayError, Result},
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use easynotes_core::ids::NoteId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const TTL: u64 = 300;
const MAX_CIPHERTEXT: usize = 4096;

#[derive(Clone)]
pub(crate) struct Pairing {
    guest_hash: Vec<u8>,
    owner_hash: Vec<u8>,
    expires: i64,
    public: Option<Vec<u8>>,
    ciphertext: Option<Vec<u8>>,
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
fn validate_id(id: &str) -> Result<()> {
    NoteId::decode(id)
        .map(|_| ())
        .map_err(|_| RelayError::BadRequest("malformed pairing id".into()))
}
fn decode(value: &str, len: usize, name: &str) -> Result<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| RelayError::BadRequest(format!("{name} is not base64url")))?;
    if bytes.len() != len {
        return Err(RelayError::BadRequest(format!(
            "{name} has the wrong length"
        )));
    }
    Ok(bytes)
}
fn has(secret: &str, hash: &[u8]) -> bool {
    URL_SAFE_NO_PAD
        .decode(secret)
        .map(|s| Sha256::digest(s).as_slice() == hash)
        .unwrap_or(false)
}
fn load(state: &AppState, id: &str) -> Result<Pairing> {
    let found = state
        .pairings
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or(RelayError::NotFound)?;
    if found.expires <= now() {
        state.pairings.lock().unwrap().remove(id);
        return Err(RelayError::NotFound);
    }
    Ok(found)
}
fn own(state: &AppState, id: &str, secret: &str) -> Result<Pairing> {
    let p = load(state, id)?;
    if has(secret, &p.owner_hash) {
        Ok(p)
    } else {
        Err(RelayError::NotFound)
    }
}
fn guest(state: &AppState, id: &str, secret: &str) -> Result<Pairing> {
    let p = load(state, id)?;
    if has(secret, &p.guest_hash) {
        Ok(p)
    } else {
        Err(RelayError::NotFound)
    }
}

#[derive(Deserialize)]
pub struct Create {
    id: String,
    guest_hash: String,
    owner_hash: String,
    expires_in: u64,
    access: Option<String>,
}
#[derive(Serialize)]
pub struct Created {
    expires_in: u64,
}
pub async fn create(
    State(s): State<Arc<AppState>>,
    Json(r): Json<Create>,
) -> Result<(StatusCode, Json<Created>)> {
    validate_id(&r.id)?;
    s.access.check_invite(&r.id, r.access.as_deref())?;
    let guest_hash = decode(&r.guest_hash, 32, "guest hash")?;
    let owner_hash = decode(&r.owner_hash, 32, "owner hash")?;
    if guest_hash == owner_hash {
        return Err(RelayError::BadRequest(
            "pairing capabilities must differ".into(),
        ));
    }
    let ttl = r.expires_in.clamp(1, TTL);
    let mut map = s.pairings.lock().unwrap();
    map.retain(|_, p| p.expires > now());
    if map.contains_key(&r.id) {
        return Err(RelayError::Conflict("pairing already exists"));
    }
    map.insert(
        r.id,
        Pairing {
            guest_hash,
            owner_hash,
            expires: now() + ttl as i64,
            public: None,
            ciphertext: None,
        },
    );
    Ok((StatusCode::CREATED, Json(Created { expires_in: ttl })))
}
#[derive(Deserialize)]
pub struct GuestRequest {
    token: String,
    public_key: String,
}
pub async fn request(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<GuestRequest>,
) -> Result<StatusCode> {
    validate_id(&id)?;
    guest(&s, &id, &r.token)?;
    let public = decode(&r.public_key, 32, "public key")?;
    let mut map = s.pairings.lock().unwrap();
    let p = map.get_mut(&id).ok_or(RelayError::NotFound)?;
    match &p.public {
        None => p.public = Some(public),
        Some(old) if *old == public => {}
        Some(_) => return Err(RelayError::Conflict("pairing already has a device request")),
    }
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
pub struct Owner {
    owner: String,
}
#[derive(Serialize)]
pub struct RequestStatus {
    public_key: Option<String>,
    expires_in: u64,
}
pub async fn poll_request(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<Owner>,
) -> Result<Json<RequestStatus>> {
    validate_id(&id)?;
    let p = own(&s, &id, &r.owner)?;
    Ok(Json(RequestStatus {
        public_key: p.public.map(|v| URL_SAFE_NO_PAD.encode(v)),
        expires_in: (p.expires - now()).max(0) as u64,
    }))
}
#[derive(Deserialize)]
pub struct Submit {
    owner: String,
    ciphertext: String,
}
pub async fn response(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<Submit>,
) -> Result<StatusCode> {
    validate_id(&id)?;
    own(&s, &id, &r.owner)?;
    let data = URL_SAFE_NO_PAD
        .decode(r.ciphertext)
        .map_err(|_| RelayError::BadRequest("ciphertext is not base64url".into()))?;
    if data.len() > MAX_CIPHERTEXT {
        return Err(RelayError::TooLarge);
    }
    let mut map = s.pairings.lock().unwrap();
    let p = map.get_mut(&id).ok_or(RelayError::NotFound)?;
    if p.public.is_none() {
        return Err(RelayError::Conflict("no device has requested pairing"));
    }
    if p.ciphertext.is_some() {
        return Err(RelayError::Conflict("pairing already approved"));
    }
    p.ciphertext = Some(data);
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
pub struct Guest {
    token: String,
}
#[derive(Serialize)]
pub struct ResponseStatus {
    ciphertext: Option<String>,
    expires_in: u64,
}
pub async fn poll_response(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<Guest>,
) -> Result<Json<ResponseStatus>> {
    validate_id(&id)?;
    guest(&s, &id, &r.token)?;
    let mut map = s.pairings.lock().unwrap();
    let p = map.get_mut(&id).ok_or(RelayError::NotFound)?;
    if !has(&r.token, &p.guest_hash) {
        return Err(RelayError::NotFound);
    }
    let response = ResponseStatus {
        ciphertext: p.ciphertext.take().map(|v| URL_SAFE_NO_PAD.encode(v)),
        expires_in: (p.expires - now()).max(0) as u64,
    };
    if response.ciphertext.is_some() {
        map.remove(&id);
    }
    Ok(Json(response))
}
pub async fn close(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<Owner>,
) -> Result<StatusCode> {
    validate_id(&id)?;
    own(&s, &id, &r.owner)?;
    s.pairings.lock().unwrap().remove(&id);
    Ok(StatusCode::NO_CONTENT)
}
