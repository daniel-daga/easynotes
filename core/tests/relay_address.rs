//! What counts as one relay address, held to the same cases as the app.
//!
//! The app keeps a separate profile for every relay, and finds a profile again
//! by the relay's canonical address. It canonicalizes in Dart before the core
//! is involved; the core does it again in [`RelayEndpoint::parse`] when it
//! connects. If the two disagreed, one relay could get two profiles, or a
//! library could be refused the relay its own profile names. So both read one
//! list of cases, and this is the core's half of it
//! (`app/test/relay_profiles_test.dart` is the app's).

#![cfg(feature = "sync-native")]

use easynotes_core::sync::RelayEndpoint;
use serde::Deserialize;

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    input: String,
    canonical: Option<String>,
}

#[test]
fn the_core_and_the_app_agree_about_what_a_relay_address_is() {
    let cases: Cases = serde_json::from_str(include_str!("fixtures/relay_addresses.json")).unwrap();
    assert!(!cases.cases.is_empty());
    for case in cases.cases {
        let parsed = RelayEndpoint::parse(&case.input).ok();
        assert_eq!(
            parsed.as_ref().map(|e| e.http_base.as_str()),
            case.canonical.as_deref(),
            "{:?}",
            case.input
        );
        // The socket's address is the same relay with the other scheme, never a
        // downgrade from TLS.
        if let (Some(endpoint), Some(canonical)) = (parsed, case.canonical) {
            let expected = canonical
                .replacen("https://", "wss://", 1)
                .replacen("http://", "ws://", 1);
            assert_eq!(endpoint.ws_base, expected, "{:?}", case.input);
        }
    }
}
