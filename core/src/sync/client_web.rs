//! The wire in a browser: the same methods as `client`, over the tab's own
//! WebSocket and `fetch`.
//!
//! A tab cannot open a TCP socket, so `tokio-tungstenite` and `reqwest` are not
//! available to it and this exists. Everything the two transports would
//! otherwise have to agree about — the messages, the base64, what counts as
//! nonsense, the `last_seq` bookkeeping — is in [`super::wire`] and
//! [`super::socket`], so what is here is the browser and nothing else.
//!
//! **Events, turned back into a future.** A `WebSocket` does not hand you the
//! next message; it calls you. So the handlers push into a channel and
//! [`Socket::recv`] awaits it, which is what lets [`super::engine`] keep
//! `select!`ing over the socket and its command queue exactly as it does on a
//! device. The channel is unbounded because dropping a relay message on the
//! floor would silently lose an edit, and a browser will run out of memory long
//! before a note's backlog is a problem.
//!
//! **Which thread this is on does not matter, as long as it is one thread.** A
//! `WebSocket` belongs to the thread that created it, and so do the handlers, so
//! everything here happens inside the one task the engine runs in. What crosses
//! threads is the engine's command channel, which is bytes and an atomic —
//! see `app/rust/src/relay.rs`.

use super::wire::{self, ChallengeResponse, TokenResponse};
use crate::crypto::DeviceSigner;
use crate::error::{CoreError, Result};
use crate::relay_access::RelayAccess;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::mpsc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{MessageEvent, Request, RequestInit, Response, WebSocket};

use wire::net;

/// A browser hides the detail of most failures — deliberately, so that a page
/// cannot probe the network it is on — so this is usually as much as there is.
fn js(e: JsValue) -> CoreError {
    net(e
        .as_string()
        .or_else(|| {
            e.dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| "the browser refused the request".to_string()))
}

/// `fetch`, from whichever global this thread has.
///
/// The engine may be running on the page or on a worker, and both have one; the
/// bindings for them are different types, which is the only reason this is not
/// one line.
fn fetch(request: &Request) -> Result<js_sys::Promise> {
    let global = js_sys::global();
    if let Some(window) = global.dyn_ref::<web_sys::Window>() {
        return Ok(window.fetch_with_request(request));
    }
    if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
        return Ok(worker.fetch_with_request(request));
    }
    Err(net("this thread has no fetch"))
}

async fn post_json<Q: Serialize, R: DeserializeOwned>(url: &str, body: &Q) -> Result<R> {
    let body = serde_json::to_string(body)
        .map_err(|e| CoreError::Storage(format!("unserializable request: {e}")))?;
    let (status, text) = post(url, body).await?;
    if let Some(outdated) = wire::refused(status, text.as_bytes()) {
        return Err(outdated);
    }
    if !(200..300).contains(&status) {
        return Err(wire::http_error(status));
    }
    serde_json::from_str(&text).map_err(|e| net(format!("relay sent nonsense: {e}")))
}

/// A JSON `POST`, and whatever came back, whatever its status.
///
/// The invite calls need the status — "expired" and "withdrawn" are answers,
/// not failures — so it is handed back rather than turned into an error here.
pub(crate) async fn post(url: &str, body: String) -> Result<(u16, String)> {
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body));
    let controller = web_sys::AbortController::new().map_err(js)?;
    init.set_signal(Some(&controller.signal()));
    let request = Request::new_with_str_and_init(url, &init).map_err(js)?;
    // Which is what makes this a preflighted request, and why the relay has to
    // answer an `OPTIONS` — see `browser_access` in the relay.
    request
        .headers()
        .set("content-type", "application/json")
        .map_err(js)?;

    let _abort = AbortOnDrop(controller.clone());
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let response: Response = JsFuture::from(fetch(&request)?)
            .await
            .map_err(js)?
            .dyn_into()
            .map_err(|_| net("fetch did not return a response"))?;
        let status = response.status();
        let text = JsFuture::from(response.text().map_err(js)?)
            .await
            .map_err(js)?
            .as_string()
            .ok_or_else(|| net("the relay's answer was not text"))?;
        Ok((status, text))
    })
    .await
    .map_err(|_| {
        controller.abort();
        net("browser request timed out after 60 seconds")
    })?
}

/// A call made as a device — its token as a bearer — and the status and body
/// that came back, whatever the status. The native client's `call`, over
/// `fetch`; what the attachment calls are made of.
///
/// Every one of these is a preflighted request, for its `authorization`
/// header alone, so the relay's CORS rules have to allow that header, `range`,
/// and `PUT` — see `browser_access` in the relay.
pub(crate) async fn call(
    method: &str,
    url: &str,
    token: &str,
    request: super::media::Request,
) -> Result<(u16, Vec<u8>, Option<std::time::Duration>)> {
    use super::media::Request as Body;
    let init = RequestInit::new();
    init.set_method(method);
    let controller = web_sys::AbortController::new().map_err(js)?;
    init.set_signal(Some(&controller.signal()));
    let content_type = match &request {
        Body::Json(body) => {
            init.set_body(&JsValue::from_str(body));
            Some("application/json")
        }
        Body::Bytes(bytes) => {
            init.set_body(&js_sys::Uint8Array::from(bytes.as_slice()));
            Some("application/octet-stream")
        }
        Body::Empty | Body::Range(..) => None,
    };
    let request_js = Request::new_with_str_and_init(url, &init).map_err(js)?;
    let headers = request_js.headers();
    headers
        .set("authorization", &format!("Bearer {token}"))
        .map_err(js)?;
    if let Some(content_type) = content_type {
        headers.set("content-type", content_type).map_err(js)?;
    }
    if let Body::Range(at, len) = request {
        headers
            .set("range", &format!("bytes={at}-{}", at + len - 1))
            .map_err(js)?;
    }

    let _abort = AbortOnDrop(controller.clone());
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let response: Response = JsFuture::from(fetch(&request_js)?)
            .await
            .map_err(js)?
            .dyn_into()
            .map_err(|_| net("fetch did not return a response"))?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("Retry-After")
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        let body = JsFuture::from(response.array_buffer().map_err(js)?)
            .await
            .map_err(js)?;
        Ok((status, js_sys::Uint8Array::new(&body).to_vec(), retry_after))
    })
    .await
    .map_err(|_| {
        controller.abort();
        net("browser request timed out after 60 seconds")
    })?
}

/// Abort an in-flight fetch when its future is cancelled or its deadline fires.
struct AbortOnDrop(web_sys::AbortController);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Prove possession of the device key and get a short-lived token.
///
/// The same two calls the native client makes, and the same order; only the HTTP
/// client differs. Note that the device key never leaves the signer: what is
/// sent is a public key and a signature over the relay's nonce.
pub async fn authenticate(
    http_base: &str,
    access: Option<&RelayAccess>,
    signer: &impl DeviceSigner,
) -> Result<String> {
    let auth = async {
        let device_key = signer.device_key()?;
        let device = device_key.encode();

        let challenge: ChallengeResponse = post_json(
            &wire::api(http_base, "/challenge"),
            &wire::ChallengeRequest { device: &device },
        )
        .await?;

        let nonce = wire::decode_challenge_nonce(&challenge.nonce)?;
        let signature = URL_SAFE_NO_PAD.encode(signer.sign_challenge(&nonce)?);

        let token: TokenResponse = post_json(
            &wire::api(http_base, "/token"),
            &wire::TokenRequest {
                device: &device,
                nonce: &challenge.nonce,
                signature: &signature,
                access: access.map(|key| key.token_proof(&nonce, &device_key.0)),
            },
        )
        .await?;

        Ok::<_, CoreError>(token.token)
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), auth)
        .await
        .map_err(|_| net("browser relay authentication timed out after 10 seconds"))?
}

/// What a handler saw.
enum Event {
    Open,
    Text(String),
    Closed,
    Failed,
}

/// The handlers, kept alive for exactly as long as the socket they are on.
///
/// A `Closure` that is dropped while still registered leaves JavaScript holding
/// a function pointer into freed Rust, so these live in the struct and are
/// unregistered in [`Socket::shut`] before they go.
struct Handlers {
    _open: Closure<dyn FnMut(web_sys::Event)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(web_sys::Event)>,
    _error: Closure<dyn FnMut(web_sys::Event)>,
}

/// A WebSocket that carries text, and nothing that knows what the text means.
pub(crate) struct Socket {
    socket: WebSocket,
    events: mpsc::UnboundedReceiver<Event>,
    handlers: Option<Handlers>,
}

impl Socket {
    pub(crate) async fn open(url: &str) -> Result<Self> {
        let socket = WebSocket::new(url).map_err(js)?;
        let (tx, events) = mpsc::unbounded_channel();

        let send = |tx: mpsc::UnboundedSender<Event>, what: fn() -> Event| {
            Closure::wrap(Box::new(move |_: web_sys::Event| {
                let _ = tx.send(what());
            }) as Box<dyn FnMut(web_sys::Event)>)
        };
        let open = send(tx.clone(), || Event::Open);
        let close = send(tx.clone(), || Event::Closed);
        let error = send(tx.clone(), || Event::Failed);
        let message = {
            let tx = tx.clone();
            Closure::wrap(Box::new(move |event: MessageEvent| {
                // Binary frames are not part of the protocol, and a relay that
                // sent one is not talking to us. Ignoring it keeps the
                // connection up, exactly as the native client does.
                if let Some(text) = event.data().as_string() {
                    let _ = tx.send(Event::Text(text));
                }
            }) as Box<dyn FnMut(MessageEvent)>)
        };
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        socket.set_onerror(Some(error.as_ref().unchecked_ref()));

        let mut sync = Self {
            socket,
            events,
            handlers: Some(Handlers {
                _open: open,
                _message: message,
                _close: close,
                _error: error,
            }),
        };

        // Nothing may be sent before the socket is open, so the first thing this
        // does is wait to be told that it is. A refused connection arrives here
        // as an error or a close rather than as a failure from `WebSocket::new`,
        // which only rejects a URL it cannot parse.
        match tokio::time::timeout(std::time::Duration::from_secs(10), sync.events.recv()).await {
            Err(_) => return Err(net("browser sync socket timed out after 10 seconds")),
            Ok(Some(Event::Open)) => {}
            Ok(Some(Event::Failed)) | Ok(None) => {
                return Err(net("the relay could not be reached"));
            }
            Ok(Some(Event::Closed)) => return Err(net("the relay closed the connection")),
            // A message before `onopen` is not something a browser does.
            Ok(Some(Event::Text(_))) => {
                return Err(net("the relay spoke before the socket opened"));
            }
        }

        Ok(sync)
    }

    pub(crate) async fn send(&mut self, text: String) -> Result<()> {
        self.socket.send_with_str(&text).map_err(js)
    }

    /// The next text frame, or `None` when the relay closes. Cancel-safe: it
    /// is a channel receive.
    pub(crate) async fn recv(&mut self) -> Result<Option<String>> {
        loop {
            return match self.events.recv().await {
                Some(Event::Text(text)) => Ok(Some(text)),
                Some(Event::Closed) | None => Ok(None),
                Some(Event::Failed) => Err(net("the connection failed")),
                // A second `onopen` cannot happen, and if it somehow did it says
                // nothing about the note.
                Some(Event::Open) => continue,
            };
        }
    }

    pub(crate) async fn close(mut self) -> Result<()> {
        self.shut();
        Ok(())
    }

    /// Unregister the handlers and close the socket, in that order.
    ///
    /// The order is the whole of it: a handler still registered when its
    /// `Closure` is freed is a function pointer into memory Rust has reclaimed,
    /// and the close itself is what fires the last event.
    fn shut(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        self.handlers = None;
        let _ = self.socket.close();
    }
}

/// Closing on drop is not tidiness, it is correctness: the engine drops a
/// socket and reconnects on every kind of failure except a clean shutdown,
/// and a socket left open would mean a tab holding one connection per
/// reconnection attempt for as long as the relay tolerated it.
impl Drop for Socket {
    fn drop(&mut self) {
        if self.handlers.is_some() {
            self.shut();
        }
    }
}
