//! data-model.md § Link and CID Formats: the CIDs Zu allows, and no others.

use crate::{Error, rule};
use multibase::Base;
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};

/// What a CID points to: a node encoded in DRISL, or the raw bytes of a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Drisl = 0x71,
    Raw = 0x55,
}

/// A CID of version 1 with a SHA-256 hash, in its binary form. It orders by those bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cid([u8; 36]);

impl Cid {
    /// The CID of `content`, hashed whole.
    pub fn of(codec: Codec, content: &[u8]) -> Cid {
        let mut bytes = [0; 36];
        bytes[..4].copy_from_slice(&[1, codec as u8, 0x12, 0x20]);
        bytes[4..].copy_from_slice(&Sha256::digest(content));
        Cid(bytes)
    }

    /// The binary form: version, codec, hash type, hash size, then the 32 bytes of the hash.
    pub fn from_bytes(bytes: &[u8]) -> Result<Cid, Error> {
        match <[u8; 36]>::try_from(bytes) {
            Ok(bytes @ [1, 0x71 | 0x55, 0x12, 0x20, ..]) => Ok(Cid(bytes)),
            _ => Err(Error::Cid),
        }
    }

    pub fn as_bytes(&self) -> &[u8; 36] {
        &self.0
    }
}

/// The string form: `b`, then base32 in lowercase without padding.
impl fmt::Display for Cid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b{}", Base::Base32Lower.encode(self.0))
    }
}

impl FromStr for Cid {
    type Err = Error;
    fn from_str(text: &str) -> Result<Cid, Error> {
        let bytes = text.strip_prefix('b').and_then(|base32| Base::Base32Lower.decode(base32).ok());
        let cid = Cid::from_bytes(&bytes.ok_or(Error::Cid)?)?;
        // One CID, one string: whatever the decoder let through must be the string we write.
        rule(cid.to_string() == text, Error::Cid).map(|()| cid)
    }
}
