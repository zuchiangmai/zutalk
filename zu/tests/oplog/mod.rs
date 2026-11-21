//! Shared by the tests that need a `did:zu`: rotation keys, and audit logs signed with them.
#![allow(dead_code)]

use multibase::Base;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use zu::{Cid, Codec, PublicKey, Value};

/// The orders of the groups of secp256k1 and of P-256, as big-endian bytes.
const ORDER_K256: &str = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141";
const ORDER_P256: &str = "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551";

/// The 64 bytes of `r` and `s` over a message, as the library makes them.
type Signer = Box<dyn Fn(&[u8]) -> Vec<u8>>;

/// A rotation key: its `did:key`, and how it signs.
pub struct Rotation {
    pub did_key: String,
    order: &'static str,
    signer: Signer,
}

/// A made-up rotation key. An odd `n` gives a secp256k1 key, an even one a P-256 key.
pub fn rotation(n: u8) -> Rotation {
    macro_rules! key {
        ($curve:ident, $code:expr, $order:expr) => {{
            use $curve::ecdsa::{Signature, SigningKey, signature::Signer};
            let key = SigningKey::from_slice(&[n; 32]).unwrap();
            let point = key.verifying_key().to_sec1_point(true);
            let multikey = Base::Base58Btc.encode([&$code[..], point.as_bytes()].concat());
            let signer = move |message: &[u8]| -> Vec<u8> {
                let sig: Signature = key.sign(message);
                sig.to_bytes().to_vec()
            };
            Rotation {
                did_key: format!("did:key:z{multikey}"),
                order: $order,
                signer: Box::new(signer),
            }
        }};
    }
    if n % 2 == 1 {
        key!(k256, [0xe7, 0x01], ORDER_K256)
    } else {
        key!(p256, [0x80, 0x24], ORDER_P256)
    }
}

impl Rotation {
    /// The 64 bytes of `r` and `s`, with `s` in the lower half of the group (`high` false) or
    /// in the upper half (`high` true): the same signature, written in its two forms.
    pub fn signature(&self, message: &[u8], high: bool) -> Vec<u8> {
        let mut sig = (self.signer)(message);
        let order = hex(self.order);
        // `s` is high if twice `s` exceeds the order: compare `s` with the order minus `s`.
        let other = minus(&order, &sig[32..]);
        if (sig[32..] > other[..]) != high {
            sig[32..].copy_from_slice(&other);
        }
        sig
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// `a - b` for big-endian numbers of the same length, `a` being the greater.
fn minus(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = vec![0; a.len()];
    let mut borrow = 0i16;
    for i in (0..a.len()).rev() {
        let digit = i16::from(a[i]) - i16::from(b[i]) - borrow;
        (out[i], borrow) = (digit.rem_euclid(256) as u8, i16::from(digit < 0));
    }
    out
}

/// An operation as a value of the data model, whose DRISL bytes are its DAG-CBOR bytes.
pub fn value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Int(n.as_i64().unwrap()),
        Json::String(text) => Value::Str(text.clone()),
        Json::Array(list) => Value::List(list.iter().map(value).collect()),
        Json::Object(map) => Value::Map(map.iter().map(|(k, v)| (k.clone(), value(v))).collect()),
    }
}

/// The CID of an operation, as `prev` and the audit log write it.
pub fn cid(op: &Json) -> String {
    Cid::of(Codec::Drisl, &value(op).encode()).to_string()
}

/// did-zu.md § Operation Serialization: add `sig`, the signature over the operation without it.
pub fn sign(mut op: Json, key: &Rotation) -> Json {
    op.as_object_mut().unwrap().remove("sig");
    let sig = Base::Base64Url.encode(key.signature(&value(&op).encode(), false));
    with(op, "sig", json!(sig))
}

/// The operation with one field set to another value.
pub fn with(mut op: Json, field: &str, to: Json) -> Json {
    op[field] = to;
    op
}

/// did-zu.md § DID Creation: the DID that a signed genesis operation makes.
pub fn did(genesis: &Json) -> String {
    let hash = Base::Base32Lower.encode(Sha256::digest(value(genesis).encode()));
    format!("did:zu:{}", &hash[..24])
}

/// A regular operation, not signed yet: rotation keys, Zu signing keys under their names,
/// `alsoKnownAs`, and the operation it follows.
pub fn operation(
    rotation: &[&Rotation],
    signing: &[(&str, PublicKey)],
    aka: &[&str],
    prev: Option<&Json>,
) -> Json {
    let methods: serde_json::Map<String, Json> = signing
        .iter()
        .map(|(name, key)| (name.to_string(), json!(format!("did:key:{key}"))))
        .collect();
    json!({
        "type": "plc_operation",
        "rotationKeys": rotation.iter().map(|key| key.did_key.clone()).collect::<Vec<_>>(),
        "verificationMethods": methods,
        "alsoKnownAs": aka,
        "services": {},
        "prev": prev.map(cid),
    })
}

/// A tombstone after `prev`, not signed yet.
pub fn tombstone(prev: &Json) -> Json {
    json!({ "type": "plc_tombstone", "prev": cid(prev) })
}

/// `YYYY-MM-DDTHH:mm:ss.sssZ` for a time in milliseconds since 1970, by the algorithm that
/// counts days forward from the epoch, which is not the one the kernel uses.
pub fn stamp(at: i64) -> String {
    let (mut days, rest) = (at.div_euclid(86_400_000), at.rem_euclid(86_400_000));
    let leap = |year: i64| year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut year = 1970;
    while days >= 365 + i64::from(leap(year)) {
        days -= 365 + i64::from(leap(year));
        year += 1;
    }
    let mut month = 1;
    for length in [31, 28 + i64::from(leap(year)), 31, 30, 31, 30, 31, 31, 30, 31, 30, 31] {
        if days < length {
            break;
        }
        (days, month) = (days - length, month + 1);
    }
    let (hour, minute, second, milli) =
        (rest / 3_600_000, rest / 60_000 % 60, rest / 1000 % 60, rest % 1000);
    format!("{year:04}-{month:02}-{:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z", days + 1)
}

/// did-zu.md § Audit Logs: the audit log of `did` as the directory serves it. Each operation
/// comes with the time it was recorded at and whether the directory marks it as nullified.
pub fn log(did: &str, ops: &[(&Json, i64, bool)]) -> Vec<u8> {
    let entries: Vec<Json> = ops
        .iter()
        .map(|(op, at, nullified)| {
            json!({ "did": did, "operation": op, "cid": cid(op), "nullified": nullified, "createdAt": stamp(*at) })
        })
        .collect();
    serde_json::to_vec(&entries).unwrap()
}
