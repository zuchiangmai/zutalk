//! Shared by the integration tests: made-up identities and small builders.
#![allow(dead_code)]

use std::collections::BTreeMap;
use zu::{Cid, Codec, Path, Record, SecretKey, Value};

/// The DIDs used as examples by the specifications.
pub const DID: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
pub const CITY: &str = "did:zu:q4cm5j2xk7hvn3oaz6t2wplb";
/// The time used as an example by the specifications.
pub const AT: i64 = 1794528000000;

pub fn secret(n: u8) -> SecretKey {
    SecretKey::from_seed(&[n; 32])
}

pub fn path(key: &str) -> Path {
    Path::new(DID, "ev/event", key).unwrap()
}

pub fn cid(content: &str) -> Cid {
    Cid::of(Codec::Drisl, content.as_bytes())
}

pub fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

pub fn map<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Map(fields.into_iter().map(|(name, value)| (name.to_owned(), value)).collect())
}

pub fn fields(value: Value) -> BTreeMap<String, Value> {
    match value {
        Value::Map(map) => map,
        other => panic!("not a map: {other:?}"),
    }
}

pub fn text(text: &str) -> Value {
    Value::Str(text.to_owned())
}

/// A record at `key` signed by `secret(1)`, with a body, following `prev`.
pub fn record(key: &str, at: i64, prev: &[&Record]) -> Record {
    let body = Some(cid(&format!("body of {key} at {at}")));
    Record::sign(&secret(1), path(key), body, prev.iter().map(|r| r.cid()), at, None).unwrap()
}

/// Sign `header` (a map without `pub` and `sig`) the way cryptography.md § Signing Data says,
/// by hand, so that tests can make headers the library itself would never make.
pub fn seal(secret: &SecretKey, header: Value) -> Value {
    let mut map = fields(header);
    map.insert("pub".to_owned(), text(&secret.public().to_string()));
    let sig = secret.sign(&Value::Map(map.clone()).encode());
    map.insert("sig".to_owned(), Value::Bytes(sig.to_vec()));
    Value::Map(map)
}

/// The fields of a record header as record.md § Header lists them, before signing.
pub fn header(key: &str) -> BTreeMap<String, Value> {
    fields(map([
        ("$type", text("record")),
        ("v", Value::Int(1)),
        ("by", text(DID)),
        ("type", text("ev/event")),
        ("key", text(key)),
        ("at", Value::Int(AT)),
        ("prev", Value::List(vec![])),
        ("body", Value::Link(cid("body"))),
    ]))
}
