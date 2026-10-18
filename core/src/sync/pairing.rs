//! One-time account transfer relay calls. The relay sees only capability
//! hashes, a temporary public key and the encrypted transfer response.
use super::{
    transport::post,
    wire::{self, invite_status},
};
use crate::{
    error::{CoreError, Result},
    relay_access::RelayAccess,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Serialize)]
struct Create<'a> {
    id: &'a str,
    guest_hash: String,
    owner_hash: String,
    expires_in: u64,
    access: Option<String>,
}
#[derive(Deserialize)]
struct Created {
    expires_in: u64,
}
#[derive(Serialize)]
struct Guest<'a> {
    token: &'a str,
}
#[derive(Serialize)]
struct Owner<'a> {
    owner: &'a str,
}
#[derive(Serialize)]
struct SubmitRequest<'a> {
    token: &'a str,
    public_key: &'a str,
}
#[derive(Serialize)]
struct SubmitResponse<'a> {
    owner: &'a str,
    ciphertext: &'a str,
}
#[derive(Deserialize)]
struct RequestStatus {
    public_key: Option<String>,
    expires_in: u64,
}
#[derive(Deserialize)]
struct ResponseStatus {
    ciphertext: Option<String>,
    expires_in: u64,
}
async fn call<Q: Serialize, R: DeserializeOwned>(url: &str, body: &Q) -> Result<R> {
    let body = serde_json::to_string(body).map_err(|e| CoreError::Storage(e.to_string()))?;
    let (status, text) = post(url, body).await?;
    invite_status(status, &text)?;
    serde_json::from_str(&text).map_err(|e| wire::net(format!("relay sent nonsense: {e}")))
}
async fn post_empty<Q: Serialize>(url: &str, body: &Q) -> Result<()> {
    let body = serde_json::to_string(body).map_err(|e| CoreError::Storage(e.to_string()))?;
    let (status, text) = post(url, body).await?;
    invite_status(status, &text)
}
fn endpoint(base: &str, id: &str, step: &str) -> String {
    wire::api(base, &format!("/pairings/{id}/{step}"))
}
pub async fn open(
    base: String,
    access: Option<RelayAccess>,
    id: String,
    guest: [u8; 32],
    owner: [u8; 32],
    ttl: u64,
) -> Result<u64> {
    use sha2::{Digest, Sha256};
    let invite = crate::ids::InviteId::decode(&id)
        .map_err(|_| CoreError::Storage("bad transfer id".into()))?;
    let result: Created = call(
        &wire::api(&base, "/pairings"),
        &Create {
            id: &id,
            guest_hash: URL_SAFE_NO_PAD.encode(Sha256::digest(guest)),
            owner_hash: URL_SAFE_NO_PAD.encode(Sha256::digest(owner)),
            expires_in: ttl,
            access: access.as_ref().map(|k| k.invite_proof(&invite)),
        },
    )
    .await?;
    Ok(result.expires_in)
}
pub async fn request(base: String, id: String, token: String, key: String) -> Result<()> {
    post_empty(
        &endpoint(&base, &id, "request"),
        &SubmitRequest {
            token: &token,
            public_key: &key,
        },
    )
    .await
}
pub async fn poll_request(
    base: String,
    id: String,
    owner: String,
) -> Result<(Option<[u8; 32]>, u64)> {
    let r: RequestStatus = call(
        &endpoint(&base, &id, "poll-request"),
        &Owner { owner: &owner },
    )
    .await?;
    let key = r
        .public_key
        .map(|k| {
            URL_SAFE_NO_PAD
                .decode(k)
                .map_err(|_| wire::net("malformed pairing key"))?
                .try_into()
                .map_err(|_| wire::net("wrong size pairing key"))
        })
        .transpose()?;
    Ok((key, r.expires_in))
}
pub async fn send_response(base: String, id: String, owner: String, data: Vec<u8>) -> Result<()> {
    post_empty(
        &endpoint(&base, &id, "response"),
        &SubmitResponse {
            owner: &owner,
            ciphertext: &URL_SAFE_NO_PAD.encode(data),
        },
    )
    .await
}
pub async fn poll_response(
    base: String,
    id: String,
    token: String,
) -> Result<(Option<Vec<u8>>, u64)> {
    let r: ResponseStatus = call(
        &endpoint(&base, &id, "poll-response"),
        &Guest { token: &token },
    )
    .await?;
    let data = r
        .ciphertext
        .map(|v| {
            URL_SAFE_NO_PAD
                .decode(v)
                .map_err(|_| wire::net("malformed transfer response"))
        })
        .transpose()?;
    Ok((data, r.expires_in))
}
pub async fn close(base: String, id: String, owner: String) -> Result<()> {
    post_empty(&endpoint(&base, &id, "close"), &Owner { owner: &owner }).await
}
