//! did.md, did-zu.md: the DIDs Zu accepts, and what Zu reads from the document of one.
//!
//! The network is not here: [`url`] says what to fetch, and [`resolve`] reads what was fetched;
//! [`create`] makes what to send. The rules of the `did:zu` operation log are those of
//! `did:plc`, after its validator <https://github.com/did-method-plc/go-didplc> (MIT or
//! Apache-2.0).

use crate::key::{Map, take};
use crate::{Cid, Codec, Error, PublicKey, Value, rule};
use multibase::Base;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// did-zu.md § Relationship to `did:plc`: the directory. A `did:zu` is resolved against no other.
pub const DIRECTORY: &str = "https://plc.zuzalu.ink";

/// did-zu.md § Key Rotation & Account Recovery: the window for undoing an operation, in ms.
pub const RECOVERY: i64 = 72 * 60 * 60 * 1000;

/// map.md § Additional Non-Syntax Restrictions: top-level domains that never resolve.
const RESERVED: &str = "alt arpa example internal invalid local localhost onion";

/// did.md § DID Documents: what Zu reads from the document of a DID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub did: String,
    /// The Zu signing keys. `None`: the key is in the document now. A time: the key was in the
    /// document, and this is when the directory recorded the operation that removed it.
    pub keys: Vec<(PublicKey, Option<i64>)>,
    /// The handles the DID claims, in lowercase, the primary one first. A claim alone proves
    /// nothing (map.md § Handle Resolution), but it says where the DID's data is looked for.
    pub handles: Vec<String>,
    /// The `service` entries by their `id` without the `#`: the `type` and the endpoint of each.
    pub services: BTreeMap<String, (String, String)>,
}

/// What a document or an operation states for Zu: signing keys, `alsoKnownAs`, services.
type Stated = (Vec<PublicKey>, Vec<String>, BTreeMap<String, (String, String)>);

impl Doc {
    /// auth.md § Verifying a Statement: `key` is a Zu signing key in the document now.
    pub fn key_now(&self, key: &PublicKey) -> bool {
        self.keys.contains(&(*key, None))
    }

    /// record.md § Keys and the DID Document: `key` is a Zu signing key now, or it was one and
    /// `at` is not later than its removal. A `did:web` keeps no history: its current keys count.
    pub fn key_at(&self, key: &PublicKey, at: i64) -> bool {
        self.keys.iter().any(|(held, until)| held == key && until.is_none_or(|until| at <= until))
    }

    /// What the document states from the time `at` on. A key it states no longer was removed
    /// then. did.md § DID Documents: every valid `zu://` entry of `alsoKnownAs` is a handle.
    fn state(&mut self, at: i64, (keys, names, services): Stated) {
        for (key, until) in &mut self.keys {
            *until = if keys.contains(key) { None } else { until.or(Some(at)) };
        }
        for key in keys {
            if !self.key_now(&key) {
                self.keys.push((key, None));
            }
        }
        let handles = names.iter().filter_map(|uri| handle(uri.strip_prefix("zu://")?));
        (self.handles, self.services) = (handles.collect(), services);
    }
}

/// did.md § DID Identifier Syntax: the method and the identifier of a DID of valid syntax.
fn parts(did: &str) -> Option<(&str, &str)> {
    let (method, id) = did.strip_prefix("did:")?.split_once(':')?;
    let named = !method.is_empty() && method.bytes().all(|b| b.is_ascii_lowercase());
    let spelled = id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:%-".contains(&b));
    let ended = !id.is_empty() && !id.ends_with([':', '%']);
    (named && spelled && ended && did.len() <= 2048).then_some((method, id))
}

/// did.md § Accepted DID Methods: a DID of valid syntax, of the method `zu` or `web`.
pub fn is_did(did: &str) -> bool {
    matches!(parts(did), Some(("zu" | "web", _)))
}

/// A device's own identity: the `did:key` of an Ed25519 Zu signing key. It names no host and
/// is in no directory, so no host takes a record by it (§ Accepted DID Methods): such a record
/// stays on the device that signed it.
pub fn local(key: &PublicKey) -> String {
    format!("did:key:{key}")
}

/// The key such a DID names; `None` for any other DID, and for a `did:key` of another curve.
pub fn local_key(did: &str) -> Option<PublicKey> {
    did.strip_prefix("did:key:")
        .and_then(|key| key.parse::<PublicKey>().ok())
        .filter(|key| local(key) == did)
}

/// `len` characters of base32, in lowercase.
fn base32(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
}

/// base64url without padding (RFC 4648 § 5), in the one form that encodes these bytes.
pub(crate) fn base64url(text: &str) -> Option<Vec<u8>> {
    Base::Base64Url.decode(text).ok().filter(|bytes| Base::Base64Url.encode(bytes) == text)
}

/// map.md § Handle Identifier Syntax: the handle in lowercase, if `text` is one.
pub fn handle(text: &str) -> Option<String> {
    let label = |part: &str| {
        let spelled = part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        (1..=63).contains(&part.len()) && spelled && !part.starts_with('-') && !part.ends_with('-')
    };
    let top = text.rsplit_once('.')?.1;
    let named = text.split('.').all(label) && !top.starts_with(|c: char| c.is_ascii_digit());
    (named && text.len() <= 253).then(|| text.to_ascii_lowercase())
}

/// path.md § `zu://` URI Structure: the origin of the host at the domain of a handle, where the
/// data of the DID that claims the handle is looked for. map.md § Additional Non-Syntax
/// Restrictions: a reserved top-level domain is refused, and so is `.test` unless `testing`.
pub fn origin(handle: &str, testing: bool) -> Result<String, Error> {
    let host = self::handle(handle).ok_or(Error::Domain)?;
    let top = host.rsplit('.').next().unwrap_or_default();
    let reserved = RESERVED.split(' ').any(|reserved| reserved == top);
    rule(!reserved && (testing || top != "test"), Error::Domain)?;
    Ok(format!("https://{host}"))
}

/// What to fetch for [`resolve`]. For a `did:zu`, its audit log at the directory (did-zu.md
/// § Audit Logs). For a `did:web`, the document at its domain (did.md § `did:web` in Zu): a
/// hostname and no path, or if `testing`, `localhost` with a port or without.
pub fn url(did: &str, testing: bool) -> Result<String, Error> {
    match parts(did).ok_or(Error::Path)? {
        // did-zu.md § Identifier Syntax: 24 characters of base32.
        ("zu", id) if base32(id, 24) => Ok(format!("{DIRECTORY}/{did}/log/audit")),
        ("zu", _) => Err(Error::Path),
        ("web", id) => {
            let port = |port: &str| port.parse::<u16>().is_ok();
            let local = id == "localhost" || id.strip_prefix("localhost%3A").is_some_and(port);
            let host = id.replace("%3A", ":");
            let origin =
                if testing && local { format!("https://{host}") } else { origin(id, testing)? };
            Ok(format!("{origin}/.well-known/did.json"))
        }
        _ => Err(Error::Method),
    }
}

/// Read what was fetched from [`url`]: the audit log of a `did:zu`, checked as did-zu.md
/// § Audit Logs says, or the document of a `did:web`.
pub fn resolve(did: &str, fetched: &[u8]) -> Result<Doc, Error> {
    let json: Json = serde_json::from_slice(fetched).map_err(|_| Error::Json)?;
    let stated = match parts(did).ok_or(Error::Path)? {
        ("zu", _) => audit(did, json.as_array().ok_or(Error::Json)?)?,
        ("web", _) => vec![(0, document(did, &json)?)],
        _ => return Err(Error::Method),
    };
    let mut doc = Doc { did: did.into(), keys: vec![], handles: vec![], services: BTreeMap::new() };
    stated.into_iter().for_each(|(at, stated)| doc.state(at, stated));
    Ok(doc)
}

/// did.md § DID Documents: a Zu signing key is named `zu`, or `zu-` and 8 characters of base32.
fn names_key(name: &str) -> bool {
    name == "zu" || name.strip_prefix("zu-").is_some_and(|tail| base32(tail, 8))
}

/// did.md § DID Documents: what a document states, as its method serves it.
fn document(did: &str, json: &Json) -> Result<Stated, Error> {
    rule(json["id"] == did, Error::Field("id"))?;
    let list = |name: &str| json[name].as_array().into_iter().flatten();
    // An `id` is a fragment, written alone or after the DID. It is compared whole.
    let fragment = |entry: &Json| {
        let id = entry["id"].as_str()?;
        id.strip_prefix(did).unwrap_or(id).strip_prefix('#').map(str::to_owned)
    };
    let keys = list("verificationMethod").filter_map(|method| {
        let ours = method["type"] == "Multikey" && method["controller"] == did;
        let key = method["publicKeyMultibase"].as_str()?.parse().ok()?;
        (ours && names_key(&fragment(method)?)).then_some(key)
    });
    let services = list("service").filter_map(|service| {
        let (kind, endpoint) = (service["type"].as_str()?, service["serviceEndpoint"].as_str()?);
        Some((fragment(service)?, (kind.to_owned(), endpoint.to_owned())))
    });
    let names = list("alsoKnownAs").filter_map(Json::as_str).map(str::to_owned);
    Ok((keys.collect(), names.collect(), services.collect()))
}

/// An operation of a `did:zu` log.
struct Op {
    cid: Cid,
    prev: Option<Cid>,
    at: i64,
    /// The rotation keys that may still sign what follows it, the highest authority first.
    keys: Vec<String>,
    /// The operation that follows it, the latest one if several did.
    next: Option<usize>,
    nullified: bool,
    stated: Stated,
}

/// did-zu.md § Audit Logs: check the audit log of `did` as the directory serves it, nullified
/// operations included. The operations left standing: when each was recorded, what it states.
fn audit(did: &str, log: &[Json]) -> Result<Vec<(i64, Stated)>, Error> {
    let mut ops: Vec<Op> = Vec::new();
    for (index, entry) in log.iter().enumerate() {
        let bad = Error::Log(index + 1);
        let (op, signed, sig) = read(did, entry).or(Err(bad))?;
        let signer = |keys: &[String]| keys.iter().position(|key| signs(key, &signed, &sig));
        match op.prev.map(|prev| ops.iter().position(|op| op.cid == prev)) {
            // The genesis operation comes first, a key of its own signed it, and it names the DID.
            None => rule(index == 0 && did == named(&op.cid) && signer(&op.keys).is_some(), bad)?,
            // It follows an operation that stands, at a later time than every operation before
            // it, and it is signed by a key that may still follow that operation.
            Some(Some(prev)) if !ops[prev].nullified && ops[index - 1].at < op.at => {
                let by = signer(&ops[prev].keys).ok_or(bad)?;
                // A recovery: whatever followed that operation is undone, if it is recent.
                let mut undone = ops[prev].next;
                rule(undone.is_none_or(|first| op.at - ops[first].at <= RECOVERY), bad)?;
                while let Some(index) = undone {
                    ops[index].nullified = true;
                    undone = ops[index].next;
                }
                // Only a key of higher authority than this one can undo this operation in turn.
                ops[prev].keys.truncate(by);
                ops[prev].next = Some(index);
            }
            _ => return Err(bad),
        }
        ops.push(op);
    }
    // The directory marks as nullified the operations that were undone, and no others.
    let marked = ops.iter().zip(log).position(|(op, entry)| entry["nullified"] != op.nullified);
    marked.map_or(Ok(()), |index| Err(Error::Log(index + 1)))?;
    rule(!ops.is_empty(), Error::Log(1))?;
    Ok(ops.into_iter().filter(|op| !op.nullified).map(|op| (op.at, op.stated)).collect())
}

/// did-zu.md § DID Creation: the DID a genesis operation makes is the start of its hash, which
/// is also the hash in its CID.
fn named(genesis: &Cid) -> String {
    format!("did:zu:{}", &Base::Base32Lower.encode(&genesis.as_bytes()[4..])[..24])
}

/// did-zu.md § How it works: a rotation key, which signs the operations of a `did:zu`. P-256,
/// which the method allows and WebCrypto has too. Its secret is made and kept by the caller;
/// here it is never printed, and it is wiped from memory when dropped.
pub struct Rotation(SigningKey);
const _: fn(SigningKey) -> Box<dyn zeroize::ZeroizeOnDrop> = |key| Box::new(key);

impl Rotation {
    /// From 32 random bytes: a scalar of P-256, which all but about one in 2^32 of them are.
    pub fn from_bytes(secret: &[u8; 32]) -> Result<Rotation, Error> {
        SigningKey::from_slice(secret).map(Rotation).map_err(|_| Error::Key)
    }

    /// Its `did:key`: the multicodec of a P-256 public key, then the compressed point.
    pub fn did_key(&self) -> String {
        let point = self.0.verifying_key().to_sec1_point(true);
        format!("did:key:z{}", Base::Base58Btc.encode([&[0x80, 0x24], point.as_bytes()].concat()))
    }

    /// The private key, for the person to save: the multicodec of a P-256 private key, then
    /// the scalar, in base58btc. The form PLC tools take a rotation key in.
    pub fn multikey(&self) -> zeroize::Zeroizing<String> {
        let bytes = zeroize::Zeroizing::new([&[0x86, 0x26][..], &self.0.to_bytes()].concat());
        zeroize::Zeroizing::new(format!("z{}", Base::Base58Btc.encode(&*bytes)))
    }

    /// Exact inverse of [`Rotation::multikey`]: parses a `z`-prefixed base58btc P-256 private
    /// multikey (`[0x86, 0x26]` + 32-byte scalar) while zeroizing intermediate buffers.
    pub fn from_multikey(text: &str) -> Result<Rotation, Error> {
        let body = text.strip_prefix('z').ok_or(Error::Key)?;
        let decoded =
            zeroize::Zeroizing::new(Base::Base58Btc.decode(body).map_err(|_| Error::Key)?);
        if decoded.len() != 34 || decoded[0] != 0x86 || decoded[1] != 0x26 {
            return Err(Error::Key);
        }
        let mut secret = zeroize::Zeroizing::new([0u8; 32]);
        secret.copy_from_slice(&decoded[2..34]);
        Self::from_bytes(&secret)
    }
}

/// did-zu.md § DID Creation: a new `did:zu` with one rotation key and one Zu signing key, and
/// no handle and no service. The signing key goes under the name did.md § DID Documents
/// derives from it. The DID, and its signed genesis operation as the JSON the directory takes
/// at `{DIRECTORY}/{did}`. The signature is deterministic (RFC 6979): the same keys, the same DID.
pub fn create(rotation: &Rotation, key: &PublicKey) -> Result<(String, String), Error> {
    create_with_aka(rotation, key, &[])
}

/// did-zu.md § DID Creation with caller-supplied `alsoKnownAs` entries (such as `zu://<handle>`).
pub fn create_with_aka(
    rotation: &Rotation,
    key: &PublicKey,
    also_known_as: &[String],
) -> Result<(String, String), Error> {
    let name = format!("zu-{}", &Base::Base32Lower.encode(Sha256::digest(key.as_bytes()))[..8]);
    let method = Value::Map(Map::from([(name, Value::Str(format!("did:key:{key}")))]));
    let aka = also_known_as.iter().cloned().map(Value::Str).collect();
    let mut op = Map::from([
        ("type".into(), Value::Str("plc_operation".into())),
        ("rotationKeys".into(), Value::List(vec![Value::Str(rotation.did_key())])),
        ("verificationMethods".into(), method),
        ("alsoKnownAs".into(), Value::List(aka)),
        ("services".into(), Value::Map(Map::new())),
        ("prev".into(), Value::Null),
    ]);
    // § Operation Serialization: over the operation without `sig`; `r` and `s`, in low-S form.
    let sig: Signature = rotation.0.sign(&Value::Map(op.clone()).encode());
    op.insert("sig".into(), Value::Str(Base::Base64Url.encode(sig.normalize_s().to_bytes())));
    let op = Value::Map(op);
    Ok((named(&Cid::of(Codec::Drisl, &op.encode())), op.to_json()?))
}

/// did-zu.md § Key Rotation: append a new Ed25519 Zu signing key to an existing `did:zu`,
/// preserving its rotation keys, handles, services, and prior verification methods.
/// Returns the signed `plc_operation` JSON and the handles currently claimed by the DID.
pub fn add_signing_key(
    did: &str,
    audit_log: &[u8],
    rotation: &Rotation,
    new_key: &PublicKey,
) -> Result<(String, Vec<String>), Error> {
    match parts(did).ok_or(Error::Path)? {
        ("zu", _) => {}
        _ => return Err(Error::Method),
    }
    let json: Json = serde_json::from_slice(audit_log).map_err(|_| Error::Json)?;
    let log = json.as_array().ok_or(Error::Json)?;
    let _ = audit(did, log)?;
    let head = log
        .iter()
        .rev()
        .find(|entry| entry["nullified"] == Json::Bool(false))
        .ok_or(Error::Log(1))?;
    let prev_cid = head["cid"].as_str().ok_or(Error::Cid)?;
    let Some(Value::Map(mut prev_map)) = plain(&head["operation"]) else {
        return Err(Error::Json);
    };
    let kind = take!(prev_map, "type", Value::Str(kind) => kind);
    let (rotation_keys, mut methods, also_known_as, services) = match kind.as_str() {
        "plc_operation" => {
            let rkeys = strings(take!(prev_map, "rotationKeys", Value::List(k) => k))?;
            let aka = strings(take!(prev_map, "alsoKnownAs", Value::List(a) => a))?;
            let methods = take!(prev_map, "verificationMethods", Value::Map(m) => m);
            let services = take!(prev_map, "services", Value::Map(s) => s);
            (rkeys, methods, aka, services)
        }
        "create" => {
            let signing = take!(prev_map, "signingKey", Value::Str(k) => k);
            let recovery = take!(prev_map, "recoveryKey", Value::Str(k) => k);
            (vec![recovery, signing], Map::new(), Vec::new(), Map::new())
        }
        _ => return Err(Error::Signer),
    };
    let rot_did = rotation.did_key();
    rule(rotation_keys.iter().any(|k| k == &rot_did), Error::Signer)?;

    let name = format!("zu-{}", &Base::Base32Lower.encode(Sha256::digest(new_key.as_bytes()))[..8]);
    methods.insert(name, Value::Str(format!("did:key:{new_key}")));
    let handles: Vec<String> =
        also_known_as.iter().filter_map(|uri| handle(uri.strip_prefix("zu://")?)).collect();
    let mut op = Map::from([
        ("type".into(), Value::Str("plc_operation".into())),
        ("rotationKeys".into(), Value::List(rotation_keys.into_iter().map(Value::Str).collect())),
        ("verificationMethods".into(), Value::Map(methods)),
        ("alsoKnownAs".into(), Value::List(also_known_as.into_iter().map(Value::Str).collect())),
        ("services".into(), Value::Map(services)),
        ("prev".into(), Value::Str(prev_cid.to_owned())),
    ]);
    let sig: Signature = rotation.0.sign(&Value::Map(op.clone()).encode());
    op.insert("sig".into(), Value::Str(Base::Base64Url.encode(sig.normalize_s().to_bytes())));
    Ok((Value::Map(op).to_json()?, handles))
}

/// did-zu.md § Operation Serialization: the operation in one entry of an audit log, the bytes
/// of it that were signed, and the signature.
fn read(did: &str, entry: &Json) -> Result<(Op, Vec<u8>, Vec<u8>), Error> {
    let Some(Value::Map(mut map)) = plain(&entry["operation"]) else { return Err(Error::Json) };
    let cid = Cid::of(Codec::Drisl, &Value::Map(map.clone()).encode());
    let at = entry["createdAt"].as_str().and_then(time).ok_or(Error::Time)?;
    rule(entry["did"] == did && entry["cid"] == cid.to_string(), Error::Cid)?;
    let sig = base64url(&take!(map, "sig", Value::Str(sig) => sig)).ok_or(Error::Signature)?;
    let signed = Value::Map(map.clone()).encode();
    // In an operation a CID is a string, and one CID has one string.
    let prev = take!(map, "prev", Value::Null => None, Value::Str(prev) => Some(prev.parse()?));
    let (keys, stated) = match take!(map, "type", Value::Str(kind) => kind).as_str() {
        "plc_operation" => regular(&mut map)?,
        // It has no rotation keys: nothing follows a tombstone, and none is the first.
        "plc_tombstone" => Default::default(),
        // The legacy creation. What it states is for atproto, and Zu reads none of it.
        "create" if prev.is_none() => {
            let signing = take!(map, "signingKey", Value::Str(key) => key);
            let recovery = take!(map, "recoveryKey", Value::Str(key) => key);
            take!(map, "handle", Value::Str(_) => ());
            take!(map, "service", Value::Str(_) => ());
            (vec![recovery, signing], Default::default())
        }
        _ => return Err(Error::Field("type")),
    };
    rule(map.is_empty(), Error::Field("type"))?;
    Ok((Op { cid, prev, at, keys, next: None, nullified: false, stated }, signed, sig))
}

/// did-zu.md § How it works: the rotation keys of a regular operation, and what it states.
fn regular(map: &mut Map) -> Result<(Vec<String>, Stated), Error> {
    let keys = strings(take!(map, "rotationKeys", Value::List(keys) => keys))?;
    let names = strings(take!(map, "alsoKnownAs", Value::List(names) => names))?;
    let (mut signing, mut services) = (Vec::new(), BTreeMap::new());
    for (name, key) in take!(map, "verificationMethods", Value::Map(methods) => methods) {
        let Value::Str(key) = key else { return Err(Error::Field("verificationMethods")) };
        // A Zu signing key is an Ed25519 `did:key` under one of the names for such keys.
        let key = key.strip_prefix("did:key:").and_then(|key| key.parse::<PublicKey>().ok());
        signing.extend(key.filter(|_| names_key(&name)));
    }
    for (id, service) in take!(map, "services", Value::Map(services) => services) {
        let Value::Map(mut service) = service else { return Err(Error::Field("services")) };
        let kind = take!(service, "type", Value::Str(kind) => kind);
        let endpoint = take!(service, "endpoint", Value::Str(endpoint) => endpoint);
        rule(service.is_empty(), Error::Field("services"))?;
        services.insert(id, (kind, endpoint));
    }
    Ok((keys, (signing, names, services)))
}

fn strings(list: Vec<Value>) -> Result<Vec<String>, Error> {
    let string = |item| if let Value::Str(text) = item { Ok(text) } else { Err(Error::Json) };
    list.into_iter().map(string).collect()
}

/// An operation is made of strings, lists, maps and null. As a value of the data model its
/// DRISL bytes are its DAG-CBOR bytes, which are what is signed and hashed.
fn plain(json: &Json) -> Option<Value> {
    Some(match json {
        Json::Null => Value::Null,
        Json::String(text) => Value::Str(text.clone()),
        Json::Array(list) => Value::List(list.iter().map(plain).collect::<Option<_>>()?),
        Json::Object(map) => {
            let fields = map.iter().map(|(name, value)| Some((name.clone(), plain(value)?)));
            Value::Map(fields.collect::<Option<_>>()?)
        }
        _ => return None,
    })
}

/// did-zu.md § Operation Serialization: `sig` is the ECDSA-SHA256 signature over `message` by
/// the rotation key `key`, a `did:key` of secp256k1 or of P-256: `r` and `s`, in low-S form.
fn signs(key: &str, message: &[u8], sig: &[u8]) -> bool {
    macro_rules! verify {
        ($curve:ident, $point:expr) => {{
            use $curve::ecdsa::{Signature, VerifyingKey, signature::Verifier};
            let (key, sig) = (VerifyingKey::from_sec1_bytes($point), Signature::from_slice(sig));
            matches!((key, sig), (Ok(key), Ok(sig))
                if sig.normalize_s() == sig && key.verify(message, &sig).is_ok())
        }};
    }
    // Such a key has 48 characters after the `z`. Asking first keeps base58 off long input.
    let key = key.strip_prefix("did:key:z").filter(|key| key.len() == 48);
    match key.and_then(|key| Base::Base58Btc.decode(key).ok()).as_deref() {
        Some([0xe7, 0x01, point @ ..]) if point.len() == 33 => verify!(k256, point),
        Some([0x80, 0x24, point @ ..]) if point.len() == 33 => verify!(p256, point),
        _ => false,
    }
}

/// did-zu.md § Bulk Export: `YYYY-MM-DDTHH:mm:ss.sssZ`, as milliseconds since 1970.
fn time(text: &str) -> Option<i64> {
    let shape: String = text.chars().map(|c| if c.is_ascii_digit() { '0' } else { c }).collect();
    let part = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (part(0, 4)?, part(5, 7)?, part(8, 10)?);
    let (hour, minute, second) = (part(11, 13)?, part(14, 16)?, part(17, 19)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [31, 28 + i64::from(leap), 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let dated = months.get((month - 1) as usize).is_some_and(|days| (1..=*days).contains(&day));
    let shaped = shape == "0000-00-00T00:00:00.000Z" && hour < 24 && minute < 60 && second < 60;
    (shaped && dated).then_some(())?;
    // Days since 1970-01-01. A year is counted from March, so that a leap day ends it.
    let year = year - i64::from(month < 3);
    let leaps = year.div_euclid(4) - year.div_euclid(100) + year.div_euclid(400);
    let days = year * 365 + leaps + (153 * ((month + 9) % 12) + 2) / 5 + day - 719469;
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + part(20, 23)?)
}
