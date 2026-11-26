//! blob.md, retrieval.md: files, the references that bodies make to them, and content fetched
//! by its CID.

use crate::key::Map;
use crate::{Cid, Codec, Error, Value, rule};

/// data-model.md § `blob` Type: a reference to a file, as the body of a record holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    /// The CID of the bytes of the file, with the `raw` codec.
    pub cid: Cid,
    /// The media type, written by whoever wrote the record. It is applied after the check.
    pub mime: String,
    /// The length in bytes. A reader stops reading content that grows past it.
    pub size: u64,
}

fn raw(cid: &Cid) -> bool {
    cid.as_bytes()[1] == Codec::Raw as u8
}

impl Blob {
    /// The reference to a file of these bytes. A media type that is not known is written
    /// `application/octet-stream`; an empty one is refused.
    pub fn of(content: &[u8], mime: &str) -> Result<Blob, Error> {
        let (cid, size) = (Cid::of(Codec::Raw, content), content.len() as u64);
        rule(!mime.is_empty(), Error::Field("mime")).map(|()| Blob { cid, mime: mime.into(), size })
    }

    /// The map of the fixed shape, to put in a body.
    pub fn value(&self) -> Value {
        let size = Value::Int(i64::try_from(self.size).unwrap_or(i64::MAX));
        let (kind, mime) = (Value::Str("blob".into()), Value::Str(self.mime.clone()));
        let fields =
            [("$type", kind), ("ref", Value::Link(self.cid)), ("mime", mime), ("size", size)];
        Value::Map(fields.into_iter().map(|(name, value)| (name.into(), value)).collect())
    }

    /// The reference that a map is, if it has the fixed shape.
    fn read(map: &Map) -> Option<Blob> {
        match (map.get("$type")?, map.get("ref")?, map.get("mime")?, map.get("size")?) {
            (Value::Str(kind), Value::Link(cid), Value::Str(mime), Value::Int(size))
                if kind == "blob" && raw(cid) && !mime.is_empty() =>
            {
                Some(Blob { cid: *cid, mime: mime.clone(), size: u64::try_from(*size).ok()? })
            }
            _ => None,
        }
    }
}

/// blob.md § Keeping and Discarding: the blob references in a body, found by their shape at
/// any depth, whatever the type of the record.
pub fn refs(body: &Value) -> Vec<Blob> {
    match body {
        Value::Map(map) => Blob::read(map).into_iter().chain(map.values().flat_map(refs)).collect(),
        Value::List(list) => list.iter().flat_map(refs).collect(),
        _ => Vec::new(),
    }
}

/// blob.md § Uploading: where to `PUT` the bytes of a file, as `application/octet-stream`,
/// once the record that refers to it was delivered. The request carries no authentication.
pub fn upload(origin: &str, cid: &Cid) -> Result<String, Error> {
    rule(raw(cid), Error::Cid).map(|()| format!("{origin}/.well-known/zu-blob/{cid}"))
}

/// retrieval.md § RASL in Brief: where the host of this origin serves the content of a CID.
/// The request is a `GET` or a `HEAD`, and carries no cookies or credentials of any kind.
pub fn rasl(origin: &str, cid: &Cid) -> String {
    format!("{origin}/.well-known/rasl/{cid}")
}

/// retrieval.md § Checking Content: `content` is what `cid` names, whoever served it. It is
/// also the check of a host on an upload (blob.md § Uploading). What fails is discarded.
pub fn check(cid: &Cid, content: &[u8]) -> Result<(), Error> {
    let codec = if raw(cid) { Codec::Raw } else { Codec::Drisl };
    rule(Cid::of(codec, content) == *cid, Error::Content)
}

/// retrieval.md § Checking Content: the block that a DRISL CID names, checked and decoded
/// strictly. The body of a record is such a block, and a map (record.md § Body).
pub fn block(cid: &Cid, content: &[u8]) -> Result<Value, Error> {
    rule(!raw(cid), Error::Cid)?;
    check(cid, content).and_then(|()| Value::decode(content))
}
