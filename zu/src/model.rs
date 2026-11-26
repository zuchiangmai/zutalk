//! data-model.md: the values Zu signs and hashes, as DRISL bytes and as JSON text.

use crate::{Cid, Error, rule};
use ipld_core::ipld::Ipld;
use multibase::Base;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// § Data Types. There is no float, an integer has 64 bits and a sign, a link is an allowed CID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Bytes(Vec<u8>),
    Link(Cid),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// The DRISL bytes of the value, the only form that is signed and hashed. It cannot fail.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    /// The library decodes; what the data model does not allow, or what does not encode
    /// back to the very same bytes, is refused.
    pub fn decode(bytes: &[u8]) -> Result<Value, Error> {
        let value = Value::admit(serde_ipld_dagcbor::from_slice(bytes).map_err(|_| Error::Drisl)?)?;
        rule(value.encode() == bytes, Error::Drisl).map(|()| value)
    }

    /// § JSON Representation, on one line. A value that would not read back the same is
    /// refused: a map with the reserved key `$link` or `$bytes`, or one nested too deep.
    pub fn to_json(&self) -> Result<String, Error> {
        let text = self.json().to_string();
        rule(Value::from_json(&text).as_ref() == Ok(self), Error::Json).map(|()| text)
    }

    /// § JSON Representation. Key order and spacing are free; a number is a 64-bit integer.
    pub fn from_json(text: &str) -> Result<Value, Error> {
        Value::unjson(serde_json::from_str(text).map_err(|_| Error::Json)?)
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Value::Null => out.push(0xf6),
            Value::Bool(b) => out.push(0xf4 | u8::from(*b)),
            Value::Int(i) if *i < 0 => head(out, 1, !*i as u64),
            Value::Int(i) => head(out, 0, *i as u64),
            Value::Str(s) => string(out, 3, s.as_bytes()),
            Value::Bytes(b) => string(out, 2, b),
            // Tag 42 on a byte string of 37 bytes: 0x00, then the binary CID.
            Value::Link(cid) => out.extend([0xd8, 42, 0x58, 37, 0].iter().chain(cid.as_bytes())),
            Value::List(list) => {
                head(out, 4, list.len() as u64);
                list.iter().for_each(|v| v.write(out));
            }
            // Keys in the order of their encodings: the shorter first, then byte by byte.
            Value::Map(map) => {
                head(out, 5, map.len() as u64);
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by_key(|(k, _)| (k.len(), *k));
                for (k, v) in entries {
                    string(out, 3, k.as_bytes());
                    v.write(out);
                }
            }
        }
    }

    fn admit(ipld: Ipld) -> Result<Value, Error> {
        Ok(match ipld {
            Ipld::Null => Value::Null,
            Ipld::Bool(b) => Value::Bool(b),
            Ipld::Integer(i) => Value::Int(i64::try_from(i).map_err(|_| Error::Drisl)?),
            Ipld::Float(_) => return Err(Error::Drisl),
            Ipld::String(s) => Value::Str(s),
            Ipld::Bytes(b) => Value::Bytes(b),
            Ipld::Link(cid) => Value::Link(Cid::from_bytes(&cid.to_bytes())?),
            Ipld::List(list) => Value::List(all(list.into_iter().map(Value::admit))?),
            Ipld::Map(map) => {
                Value::Map(all(map.into_iter().map(|(k, v)| Ok((k, Value::admit(v)?))))?)
            }
        })
    }

    fn json(&self) -> Json {
        match self {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Int(i) => Json::from(*i),
            Value::Str(s) => Json::from(s.as_str()),
            Value::Bytes(b) => json!({ "$bytes": Base::Base64.encode(b) }),
            Value::Link(cid) => json!({ "$link": cid.to_string() }),
            Value::List(list) => Json::Array(list.iter().map(Value::json).collect()),
            Value::Map(map) => map.iter().map(|(k, v)| (k.clone(), v.json())).collect(),
        }
    }

    fn unjson(json: Json) -> Result<Value, Error> {
        Ok(match json {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(b),
            Json::Number(n) => Value::Int(n.as_i64().ok_or(Error::Json)?),
            Json::String(s) => Value::Str(s),
            Json::Array(list) => Value::List(all(list.into_iter().map(Value::unjson))?),
            // § `link` and § `bytes`: an object with that single key, and a string under it.
            Json::Object(map) => match (map.get("$link"), map.get("$bytes"), map.len()) {
                (Some(Json::String(cid)), None, 1) => Value::Link(cid.parse()?),
                // Plain base64, with `=` padding or without.
                (None, Some(Json::String(s)), 1) => {
                    let bytes = Base::Base64.decode(s).or_else(|_| Base::Base64Pad.decode(s));
                    Value::Bytes(bytes.map_err(|_| Error::Json)?)
                }
                (None, None, _) => {
                    Value::Map(all(map.into_iter().map(|(k, v)| Ok((k, Value::unjson(v)?))))?)
                }
                _ => return Err(Error::Json),
            },
        })
    }
}

fn all<T, C: FromIterator<T>>(items: impl Iterator<Item = Result<T, Error>>) -> Result<C, Error> {
    items.collect()
}

/// A CBOR head: the major type, and its argument in the shortest form that holds it.
fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let (info, skip) = match n {
        0..=23 => (n as u8, 8),
        24..=0xff => (24, 7),
        0x100..=0xffff => (25, 6),
        0x1_0000..=0xffff_ffff => (26, 4),
        _ => (27, 0),
    };
    out.push(major << 5 | info);
    out.extend_from_slice(&n.to_be_bytes()[skip..]);
}

/// A byte string (major type 2) or a text string (3): its length, then its bytes.
fn string(out: &mut Vec<u8>, major: u8, bytes: &[u8]) {
    head(out, major, bytes.len() as u64);
    out.extend_from_slice(bytes);
}
