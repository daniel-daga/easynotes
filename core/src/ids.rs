//! Opaque 128-bit identifiers. The relay sees note IDs; it learns nothing from them.

use crate::error::{CoreError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

macro_rules! id_type {
    ($name:ident, $what:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub [u8; 16]);

        impl $name {
            pub fn random() -> Self {
                let mut b = [0u8; 16];
                crate::crypto::random_bytes(&mut b);
                Self(b)
            }

            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            pub fn from_bytes(b: [u8; 16]) -> Self {
                Self(b)
            }

            /// base64url, no padding: 22 characters.
            pub fn encode(&self) -> String {
                URL_SAFE_NO_PAD.encode(self.0)
            }

            pub fn decode(s: &str) -> Result<Self> {
                let raw = URL_SAFE_NO_PAD.decode(s).map_err(|_| {
                    CoreError::MalformedShareBlob(concat!($what, " is not base64url"))
                })?;
                let b: [u8; 16] = raw.try_into().map_err(|_| {
                    CoreError::MalformedShareBlob(concat!($what, " is not 16 bytes"))
                })?;
                Ok(Self(b))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.encode())
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", $what, self.encode())
            }
        }
    };
}

id_type!(NoteId, "NoteId");
id_type!(ItemId, "ItemId");
