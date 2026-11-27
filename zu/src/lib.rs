//! The Zu protocol kernel. See <https://github.com/zuchiangmai/zu> for the specifications.
//!
//! Everything but [`pack`] is a pure rule: no IO, no clock, no randomness, no global state.
//! A time is a parameter, in milliseconds since 1970-01-01T00:00:00Z.
#![forbid(unsafe_code)]

pub mod auth;
pub mod blob;
pub mod cid;
pub mod city;
pub mod did;
pub mod key;
pub mod login;
pub mod model;
pub mod pack;
pub mod path;
pub mod query;
pub mod record;

pub use crate::{blob::Blob, cid::Cid, cid::Codec, did::Doc, key::PublicKey, key::SecretKey};
pub use crate::{model::Value, pack::Held, pack::Pack, path::Path, query::Query, record::Record};

/// Why something was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Drisl,                  // bytes that are not DRISL as the Zu data model allows it
    Json,                   // JSON text that is not a value of the model, or a value it cannot hold
    Cid,                    // a CID outside the allowed set
    Key,                    // a public key that is not canonical, of small order, or no multikey
    Signature,              // a signature that does not pass the strict rules
    Path,                   // a DID, a type, a key or a `zu://` URI of the wrong syntax
    Method,                 // a DID of valid syntax, of a method Zu does not accept
    Domain,                 // a domain that is no handle, is reserved, or is for testing only
    Url,                    // a URL that is not of the form its specification gives
    Log(usize),             // an operation of a `did:zu` log, counted from 1, that breaks its rules
    Field(&'static str),    // a field that is missing, of the wrong type, or not the one expected
    Signer,                 // `pub` is not a Zu signing key of `by`
    Time,                   // a time out of order, or out of range
    Audience,               // a statement made for another host
    Replay,                 // a statement whose nonce was seen before
    Sealed,                 // a sealed message that does not open: another key or room, or altered
    Content,                // content that is not what its CID says
    Io(std::io::ErrorKind), // a file operation failed
    Corrupt(usize),         // a line of a pack, counted from 1, neither a record nor the tail
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "zu: {self:?}")
    }
}
impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Io(error.kind())
    }
}

/// `Ok` if the rule holds, the error if it does not.
pub(crate) fn rule(holds: bool, error: Error) -> Result<(), Error> {
    if holds { Ok(()) } else { Err(error) }
}
