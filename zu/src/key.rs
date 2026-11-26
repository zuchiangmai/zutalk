//! cryptography.md: Zu signing keys. Ed25519, checked by the strict rules.

use crate::{Error, Value, rule};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use multibase::Base;
use std::{collections::BTreeMap, fmt, str::FromStr};

/// The group order `L` of RFC 8032 § 5.1, as its high and its low 128 bits.
const L: (u128, u128) = (1 << 124, 0x14def9de_a2f79cd6_5812631a_5cf5d3ed);

/// RFC 8032 § 5.1.3: an encoded point is canonical if its `y` is below the prime 2^255 - 19.
fn canonical(point: &[u8]) -> bool {
    !matches!(point, [low, mid @ .., high]
        if *low >= 0xed && mid.iter().all(|&b| b == 0xff) && high | 0x80 == 0xff)
}

/// § Verifying Signatures: 32 bytes, the canonical encoding of a point not of small order.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<PublicKey, Error> {
        match <&[u8; 32]>::try_from(bytes).map(VerifyingKey::from_bytes) {
            Ok(Ok(key)) if canonical(bytes) && !key.is_weak() => Ok(PublicKey(key)),
            _ => Err(Error::Key),
        }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// § Verifying Signatures: every rule, whichever of them the library applies by itself.
    pub fn verify(&self, message: &[u8], sig: &[u8]) -> Result<(), Error> {
        let sig = <&[u8; 64]>::try_from(sig).map_err(|_| Error::Signature)?;
        let half = |from: usize| u128::from_le_bytes(std::array::from_fn(|i| sig[from + i]));
        // `R` is a canonical encoding, and `S` is below `L`.
        rule(canonical(&sig[..32]) && (half(48), half(32)) < L, Error::Signature)?;
        // The library: `R` is not of small order, and `[S]B = R + [k]A` without the cofactor.
        self.0.verify_strict(message, &Signature::from_bytes(sig)).map_err(|_| Error::Signature)
    }
}

/// § Public Key Encoding: multikey, `z` and the base58btc of 0xed 0x01 and the key.
impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "z{}", Base::Base58Btc.encode([&[0xed, 0x01], &self.as_bytes()[..]].concat()))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl FromStr for PublicKey {
    type Err = Error;
    fn from_str(text: &str) -> Result<PublicKey, Error> {
        // An Ed25519 multikey has 48 characters. Asking first keeps base58 off long input.
        let base58 = text.strip_prefix('z').filter(|_| text.len() == 48).ok_or(Error::Key)?;
        let key = match Base::Base58Btc.decode(base58).as_deref() {
            Ok([0xed, 0x01, key @ ..]) => PublicKey::from_bytes(key)?,
            _ => return Err(Error::Key),
        };
        // One key, one string, so that keys can be compared as strings.
        rule(key.to_string() == text, Error::Key).map(|()| key)
    }
}

/// The secret half of a Zu signing key. It is never printed, serialized or handed back, and
/// it is wiped from memory when dropped: the line below does not compile otherwise.
pub struct SecretKey(SigningKey);
const _: fn(SigningKey) -> Box<dyn zeroize::ZeroizeOnDrop> = |key| Box::new(key);

impl SecretKey {
    /// From the 32-byte seed of RFC 8032 § 5.1.5, which the caller makes and keeps.
    pub fn from_seed(seed: &[u8; 32]) -> SecretKey {
        SecretKey(SigningKey::from_bytes(seed))
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    /// § Signature System: plain Ed25519 over the message as it is.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.0.sign(message).to_bytes()
    }
}

pub(crate) type Map = BTreeMap<String, Value>;

/// Take a field out of a map of signed data. It must be there, and of one of the shapes.
macro_rules! take {
    ($map:expr, $name:literal, $($shape:pat $(if $guard:expr)? => $value:expr),+) => {
        match $map.remove($name) {
            $(Some($shape) $(if $guard)? => $value,)+
            _ => return Err(Error::Field($name)),
        }
    };
}
pub(crate) use take;

/// § Signing Data: a map with its `$type`, `v` and `pub`. Without `sig` it is what is signed.
pub(crate) fn sealed(kind: &str, key: &PublicKey, sig: Option<&[u8; 64]>, mut map: Map) -> Value {
    map.insert("$type".into(), Value::Str(kind.into()));
    map.insert("v".into(), Value::Int(1));
    map.insert("pub".into(), Value::Str(key.to_string()));
    map.extend(sig.map(|sig| ("sig".into(), Value::Bytes(sig.to_vec()))));
    Value::Map(map)
}

/// § Signing Data: check `$type` and `v` before anything else, then take out `pub` and `sig`.
pub(crate) fn unseal(value: Value, kind: &str) -> Result<(Map, PublicKey, [u8; 64]), Error> {
    let Value::Map(mut map) = value else { return Err(Error::Field("$type")) };
    take!(map, "$type", Value::Str(t) if t == kind => ());
    take!(map, "v", Value::Int(1) => ());
    let key = take!(map, "pub", Value::Str(key) => key).parse()?;
    let sig = take!(map, "sig", Value::Bytes(sig) => sig);
    Ok((map, key, sig.try_into().map_err(|_| Error::Field("sig"))?))
}
