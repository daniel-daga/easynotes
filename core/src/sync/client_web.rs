//! The wire in a browser: the same four methods as `client`, over the tab's own
//! WebSocket and `fetch`.
//!
//! A tab cannot open a TCP socket, so `tokio-tungstenite` and `reqwest` are not
//! available to it and this exists. Everything the two transports would
//! otherwise have to agree about — the messages, the base64, what counts as
//! nonsense, the `last_seq` bookkeeping — is in [`super::wire`], so what is here
//! is the browser and nothing else.
//!
//! **Events, turned back into a future.** A `WebSocket` does not hand you the
//! next message; it calls you. So the handlers push into a channel and
//! [`NoteSync::next`] awaits it, which is what lets [`super::engine`] keep
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

use super::wire::{self, ChallengeResponse, Incoming, TokenResponse};
use crate::crypto::DeviceSigner;
use crate::error::{CoreError, Result};
use crate::ids::NoteId;
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
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body));
    let request = Request::new_with_str_and_init(url, &init).map_err(js)?;
    // Which is what makes this a preflighted request, and why the relay has to
    // answer an `OPTIONS` — see `browser_access` in the relay.
    request
        .headers()
        .set("content-type", "application/json")
        .map_err(js)?;

    let response: Response = JsFuture::from(fetch(&request)?)
        .await
        .map_err(js)?
        .dyn_into()
        .map_err(|_| net("fetch did not return a response"))?;
    if !response.ok() {
        return Err(net(format!(
            "{} {}",
            response.status(),
            response.status_text()
        )));
    }
    let text = JsFuture::from(response.text().map_err(js)?)
        .await
        .map_err(js)?
        .as_string()
        .ok_or_else(|| net("the relay's answer was not text"))?;
    serde_json::from_str(&text).map_err(|e| net(format!("relay sent nonsense: {e}")))
}

/// Prove possession of the device key and get a short-lived token.
///
/// The same two calls the native client makes, and the same order; only the HTTP
/// client differs. Note that the device key never leaves the signer: what is
/// sent is a public key and a signature over the relay's nonce.
pub async fn authenticate(http_base: &str, signer: &impl DeviceSigner) -> Result<String> {
    let device = signer.device_key()?.encode();

    let challenge: ChallengeResponse = post_json(
        &format!("{http_base}/v1/challenge"),
        &wire::ChallengeRequest { device: &device },
    )
    .await?;

    let nonce = URL_SAFE_NO_PAD
        .decode(&challenge.nonce)
        .map_err(|_| CoreError::Storage("relay sent a malformed nonce".into()))?;
    let signature = URL_SAFE_NO_PAD.encode(signer.sign_challenge(&nonce)?);

    let token: TokenResponse = post_json(
        &format!("{http_base}/v1/token"),
        &wire::TokenRequest {
            device: &device,
            nonce: &challenge.nonce,
            signature: &signature,
        },
    )
    .await?;

    Ok(token.token)
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
/// unregistered in [`NoteSync::shut`] before they go.
struct Handlers {
    _open: Closure<dyn FnMut(web_sys::Event)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(web_sys::Event)>,
    _error: Closure<dyn FnMut(web_sys::Event)>,
}

/// A live connection to one note.
pub struct NoteSync {
    socket: WebSocket,
    events: mpsc::UnboundedReceiver<Event>,
    handlers: Option<Handlers>,
    /// The highest sequence number applied. Survives a reconnect: it is what the
    /// next `subscribe` replays from.
    pub last_seq: i64,
}

impl NoteSync {
    /// Connect and subscribe from `since`. Pass the `last_seq` from a previous
    /// connection to resume; pass 0 for the whole log.
    pub async fn connect(ws_base: &str, token: &str, note: NoteId, since: i64) -> Result<Self> {
        let socket = WebSocket::new(&wire::note_url(ws_base, note, token)).map_err(js)?;
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
            last_seq: since,
        };

        // Nothing may be sent before the socket is open, so the first thing this
        // does is wait to be told that it is. A refused connection arrives here
        // as an error or a close rather than as a failure from `WebSocket::new`,
        // which only rejects a URL it cannot parse.
        match sync.events.recv().await {
            Some(Event::Open) => {}
            Some(Event::Failed) | None => return Err(net("the relay could not be reached")),
            Some(Event::Closed) => return Err(net("the relay closed the connection")),
            // A message before `onopen` is not something a browser does.
            Some(Event::Text(_)) => return Err(net("the relay spoke before the socket opened")),
        }

        sync.socket
            .send_with_str(&wire::subscribe(since))
            .map_err(js)?;
        Ok(sync)
    }

    /// Hand an envelope to the relay. It comes back through `next` like any other,
    /// which is what keeps the local and remote paths identical.
    pub async fn append(&mut self, envelope: &[u8]) -> Result<()> {
        self.socket
            .send_with_str(&wire::append(envelope))
            .map_err(js)
    }

    /// The next thing from the relay, or `None` when it closes.
    pub async fn next(&mut self) -> Result<Option<Incoming>> {
        loop {
            return match self.events.recv().await {
                Some(Event::Text(text)) => wire::parse(&text, &mut self.last_seq).map(Some),
                Some(Event::Closed) | None => Ok(None),
                Some(Event::Failed) => Err(net("the connection failed")),
                // A second `onopen` cannot happen, and if it somehow did it says
                // nothing about the note.
                Some(Event::Open) => continue,
            };
        }
    }

    pub async fn close(mut self) -> Result<()> {
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
/// `NoteSync` and reconnects on every kind of failure except a clean shutdown,
/// and a socket left open would mean a tab holding one connection per
/// reconnection attempt for as long as the relay tolerated it.
impl Drop for NoteSync {
    fn drop(&mut self) {
        if self.handlers.is_some() {
            self.shut();
        }
    }
}
