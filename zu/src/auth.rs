//! auth.md: the statement by which software proves to a host which DID it acts for.

use crate::key::{Map, sealed, take, unseal};
use crate::{Error, PublicKey, SecretKey, Value, rule};
use multibase::Base;

/// § Verifying a Statement: `at` counts within 5 minutes of the host's clock, either way. A
/// host remembers accepted nonces for at least twice as long.
pub const WINDOW: u64 = 5 * 60 * 1000;

/// § Statement, § Sending a Statement: sign a statement that `key` acts for `by` towards
/// `aud`, the origin of the URL the request goes to, and return the value of the request's
/// `Authorization` header. `nonce` is at least 16 random bytes, made anew for each statement.
pub fn sign(key: &SecretKey, by: &str, aud: &str, at: i64, nonce: &[u8]) -> Result<String, Error> {
    rule(nonce.len() >= 16, Error::Field("nonce"))?;
    let mut map = Map::new();
    map.insert("by".into(), Value::Str(by.into()));
    map.insert("aud".into(), Value::Str(aud.into()));
    map.insert("at".into(), Value::Int(at));
    map.insert("nonce".into(), Value::Bytes(nonce.into()));
    let sig = key.sign(&sealed("auth", &key.public(), None, map.clone()).encode());
    let statement = sealed("auth", &key.public(), Some(&sig), map);
    Ok(format!("Zu {}", Base::Base64Url.encode(statement.encode())))
}

/// § Verifying a Statement: the DID that a request with this `Authorization` header acts for,
/// to a host that received it under the origin `aud` when its clock said `now`.
/// `key_now(by, pub)`: `pub` is a Zu signing key in the current DID document of `by`.
/// `unseen(by, nonce)`: the host has not seen this nonce with this `by`, and remembers it from
/// now on. The memory is the host's. It is asked last, of statements that pass all the rest.
pub fn verify(
    header: &str,
    aud: &str,
    now: i64,
    key_now: impl FnOnce(&str, &PublicKey) -> bool,
    unseen: impl FnOnce(&str, &[u8]) -> bool,
) -> Result<String, Error> {
    let base64url = header.strip_prefix("Zu ").and_then(|text| Base::Base64Url.decode(text).ok());
    let statement = Value::decode(&base64url.ok_or(Error::Field("Authorization"))?)?;
    let (mut map, key, sig) = unseal(statement, "auth")?;
    let signed = sealed("auth", &key, None, map.clone()).encode();
    let by = take!(map, "by", Value::Str(by) => by);
    let to = take!(map, "aud", Value::Str(aud) => aud);
    let at = take!(map, "at", Value::Int(at) => at);
    let nonce = take!(map, "nonce", Value::Bytes(nonce) if nonce.len() >= 16 => nonce);
    rule(to == aud, Error::Audience)?;
    rule(at.abs_diff(now) <= WINDOW, Error::Time)?;
    key.verify(&signed, &sig)?;
    rule(key_now(&by, &key), Error::Signer)?;
    rule(unseen(&by, &nonce), Error::Replay).map(|()| by)
}
