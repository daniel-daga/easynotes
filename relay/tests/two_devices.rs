//! M3 acceptance: the demo, end to end, through a real relay over a real socket.
//!
//! These are demo steps 1–5 from the initial implementation, run headlessly so that they
//! are in CI rather than in someone's memory of a meeting. Step 6 (relaunch with
//! no network) lives in `core/tests/persistence.rs`, which needs no relay.

mod support;

use easynotes_core::store::OpStore;
use easynotes_relay::{MemoryStore, Relay};
use std::sync::Arc;
use support::{contains, Device, Tap};

/// A relay with a wire tap in front of it. Devices are handed the tap's address,
/// so every byte they exchange with the relay is captured.
async fn tapped_relay() -> (Relay, MemoryStore, Tap) {
    let store = MemoryStore::new();
    let relay = Relay::serve(Arc::new(store.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let tap = Tap::in_front_of(relay.addr).await;
    (relay, store, tap)
}

async fn two_devices() -> (Relay, MemoryStore, Tap, Device, Device) {
    let (relay, store, tap) = tapped_relay().await;
    let a = Device::owner("A", &tap.http_base(), &tap.ws_base()).await;
    let b = Device::guest("B", &a.share_blob(), &tap.http_base(), &tap.ws_base()).await;
    (relay, store, tap, a, b)
}

/// Demo steps 1 and 2: A makes a list, B joins from the share blob and sees it.
#[tokio::test]
async fn a_guest_joining_from_a_share_blob_sees_the_list() {
    let (_relay, _store, tap, mut a, _) = two_devices().await;

    a.set_title("Groceries").await;
    for item in ["milk", "bread", "eggs"] {
        a.add_item(item).await;
    }
    a.pump().await;

    // B joins *after* the edits, so this also covers backlog replay for a device
    // that was never connected.
    let mut b = Device::guest("B", &a.share_blob(), &tap.http_base(), &tap.ws_base()).await;
    b.pump().await;

    assert_eq!(b.texts(), ["milk", "bread", "eggs"]);
    assert_eq!(b.view().title, "Groceries");
    assert_eq!(a.view(), b.view());
}

/// Demo step 3: B checks an item, A sees it checked.
#[tokio::test]
async fn a_check_on_one_device_shows_on_the_other() {
    let (_relay, _store, _tap, mut a, mut b) = two_devices().await;

    a.set_title("Groceries").await;
    for item in ["milk", "bread", "eggs"] {
        a.add_item(item).await;
    }
    a.pump().await;
    b.pump().await;
    assert_eq!(b.texts(), ["milk", "bread", "eggs"]);

    b.set_checked(b.item_called("bread"), true).await;
    b.pump().await;
    a.pump().await;

    let view = a.view();
    assert!(
        view.item(a.item_called("bread")).unwrap().checked,
        "A did not see the check"
    );
    assert_eq!(view, b.view());
}

/// Demo step 4, the one that matters. Both devices go offline, both edit, both
/// come back, and they converge with nothing lost and nothing duplicated.
#[tokio::test]
async fn both_devices_edit_offline_and_converge() {
    let (_relay, _store, _tap, mut a, mut b) = two_devices().await;

    a.set_title("Groceries").await;
    for item in ["milk", "bread", "eggs"] {
        a.add_item(item).await;
    }
    a.pump().await;
    b.pump().await;
    assert_eq!(a.view(), b.view());

    // --- airplane mode, both sides ---
    a.go_offline().await;
    b.go_offline().await;
    assert!(!a.is_online() && !b.is_online());

    a.add_item("coffee").await;

    let eggs = b.item_called("eggs");
    b.set_item_text(eggs, "eggs (12)").await;
    b.move_item(eggs, 0).await;

    // Offline edits are visible locally straight away.
    assert!(a.texts().contains(&"coffee".to_string()));
    assert_eq!(b.texts()[0], "eggs (12)");
    // ...and invisible to each other.
    assert!(!b.texts().contains(&"coffee".to_string()));

    // --- back online ---
    a.go_online().await;
    b.go_online().await;
    a.pump().await;
    b.pump().await;
    a.pump().await;

    assert_eq!(a.view(), b.view(), "the two devices did not converge");

    let texts = a.texts();
    assert_eq!(texts.len(), 4, "expected four items, got {texts:?}");
    assert!(
        texts.contains(&"coffee".to_string()),
        "A's offline add was lost: {texts:?}"
    );
    assert_eq!(
        texts.iter().filter(|t| t.starts_with("eggs")).count(),
        1,
        "eggs was duplicated: {texts:?}"
    );
    assert_eq!(
        texts[0], "eggs (12)",
        "B's rename and reorder did not both survive: {texts:?}"
    );
}

/// Demo step 5: nothing the relay stores or sends contains anything a user typed.
#[tokio::test]
async fn the_relay_never_sees_plaintext() {
    let (_relay, store, tap, mut a, mut b) = two_devices().await;

    // Distinctive strings, so a match cannot be a coincidence.
    let secrets = [
        "Groceries",
        "asparagus",
        "sourdough",
        "cortado",
        "eggs (12)",
    ];

    a.set_title(secrets[0]).await;
    for item in &secrets[1..4] {
        a.add_item(item).await;
    }
    a.pump().await;
    b.pump().await;
    let item = b.item_called("asparagus");
    b.set_item_text(item, secrets[4]).await;
    b.set_checked(item, true).await;
    b.pump().await;
    a.pump().await;
    assert_eq!(a.view(), b.view());

    // 1. Nothing the relay stored.
    let stored = store.all_envelopes();
    assert!(
        !stored.is_empty(),
        "the test proved nothing: the relay stored nothing"
    );
    for envelope in &stored {
        for secret in secrets {
            assert!(
                !contains(envelope, secret.as_bytes()),
                "{secret:?} is stored in plaintext on the relay"
            );
        }
    }

    // 2. Nothing the relay sent. Server-to-client WebSocket frames are unmasked,
    //    so this is the bytes of every update exactly as they crossed the wire.
    let on_the_wire = tap.bytes_to_clients();
    assert!(!on_the_wire.is_empty(), "the tap captured nothing");
    for secret in secrets {
        assert!(
            !contains(&on_the_wire, secret.as_bytes()),
            "{secret:?} crossed the wire in plaintext"
        );
    }

    // 3. Nor anything in the other direction. Client frames are XOR-masked, so
    //    this check is weaker on its own — check 1 is what covers that content.
    let to_server = tap.bytes_to_server();
    for secret in secrets {
        assert!(
            !contains(&to_server, secret.as_bytes()),
            "{secret:?} was uploaded in plaintext"
        );
    }

    // 4. The relay's keys are opaque IDs, not anything a user chose.
    for note_id in store.note_ids() {
        for secret in secrets {
            assert!(!note_id.contains(secret), "a note ID leaks {secret:?}");
        }
        assert_eq!(
            note_id.len(),
            22,
            "a note ID is a 128-bit opaque value and nothing else"
        );
    }
}

/// A relay that hands over a tampered envelope is caught by the receiving client,
/// not trusted because it came from the server.
#[tokio::test]
async fn a_client_rejects_an_envelope_the_relay_tampered_with() {
    let (_relay, _store, _tap, mut a, mut b) = two_devices().await;
    a.add_item("milk").await;
    a.pump().await;
    b.pump().await;

    // Take a real envelope off A's store and corrupt it, as a hostile relay would.
    let mut forged = a.core.store().load(a.note).unwrap().pop().unwrap();
    let last = forged.len() - 1;
    forged[last] ^= 0xFF;

    let err = b.core.apply_remote(b.note, &forged).unwrap_err();
    assert!(
        matches!(err, easynotes_core::CoreError::BadSignature),
        "expected a signature failure, got {err}"
    );
    // ...and the document is untouched.
    assert_eq!(b.texts(), ["milk"]);
}

/// Three devices on one note, which nothing in the design forbids.
#[tokio::test]
async fn three_devices_converge_through_the_relay() {
    let (_relay, _store, tap, mut a, mut b) = two_devices().await;
    let mut c = Device::guest("C", &a.share_blob(), &tap.http_base(), &tap.ws_base()).await;

    a.add_item("milk").await;
    b.add_item("bread").await;
    c.add_item("eggs").await;

    for _ in 0..2 {
        a.pump().await;
        b.pump().await;
        c.pump().await;
    }

    assert_eq!(a.view(), b.view());
    assert_eq!(b.view(), c.view());
    assert_eq!(a.texts().len(), 3, "got {:?}", a.texts());
}
