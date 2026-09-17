//! A deliberately minimal cdylib whose only job is to be weighed.
//!
//! Built once per feature set, the difference between the resulting `.wasm` files
//! is what each CRDT backend costs a web user on first load. The exported
//! functions exist so the linker cannot discard the code we are trying to measure.

use easynotes_core::crypto::{DeviceIdentity, Envelope, NoteKey};
use easynotes_core::ids::NoteId;

/// The baseline every build shares: identity, envelope seal and open.
#[no_mangle]
pub extern "C" fn crypto_round_trip() -> u32 {
    let identity = DeviceIdentity::generate();
    let note = NoteId::random();
    let key = NoteKey::random();
    let env = Envelope::seal(&identity, note, &key, 0, b"milk").expect("seal");
    env.open(&key).expect("open").len() as u32
}

#[cfg(feature = "automerge-backend")]
#[no_mangle]
pub extern "C" fn automerge_round_trip() -> u32 {
    run::<easynotes_core::crdt::AutomergeNote>()
}

#[cfg(feature = "yrs-backend")]
#[no_mangle]
pub extern "C" fn yrs_round_trip() -> u32 {
    run::<easynotes_core::crdt::YrsNote>()
}

#[cfg(any(feature = "automerge-backend", feature = "yrs-backend"))]
fn run<D: easynotes_core::crdt::NoteDoc>() -> u32 {
    use easynotes_core::crdt::Edit;
    use easynotes_core::ids::ItemId;

    let mut a = D::create(&[1u8; 16]);
    let update = a
        .apply_local(&Edit::AddItem {
            id: ItemId::random(),
            text: "milk".into(),
            order: easynotes_core::order::first_key(),
        })
        .expect("local edit");

    let mut b = D::create(&[2u8; 16]);
    b.apply_remote(&update).expect("remote update");
    let restored = D::load(&[3u8; 16], &b.snapshot()).expect("snapshot");
    restored.view().items.len() as u32
}
