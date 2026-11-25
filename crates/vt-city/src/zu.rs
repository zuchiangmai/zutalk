//! Pol V0.3 (Zu protocol v0.3.x): DRISL (deterministic CBOR), DASL CIDv1,
//! `zu-login-1` ECDH device linking with `pol-login-check-1` 6-digit check,
//! signed Zu records, `did:zu` document verification, and V0.3 governance,
//! calendar, channel, and recording archive reductions.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::calendar::CityEvent;
use crate::channels::Channel;
use crate::ed;
use crate::error::{CityError, Result};
use crate::hub::{ROLE_COORDINATOR, ROLE_CURATOR, ROLE_INVITER, ROLE_RESIDENT};
use crate::publish::{PartInput, Sentence, TakeInput, Translated, MIME_M4A, SENTENCE_MAX};
use crate::text::{b64url, base58, js_trim, utf16_prefix};

pub const CODEC_RAW: u8 = 0x55;
pub const CODEC_DRISL: u8 = 0x71;
const MAX_CBOR_DEPTH: usize = 64;
const LOGIN_INFO: &[u8] = b"zu-login-1";
const LOGIN_CHECK_DOMAIN: &[u8] = b"pol-login-check-1";
const PROPOSAL_MS: i64 = 14 * 24 * 60 * 60 * 1_000;

const B32_LOWER: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

// =========================================================================
// Base32lower & DASL CIDv1
// =========================================================================

pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buf = 0u32;
    let mut bits = 0u8;
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32_LOWER[((buf >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32_LOWER[((buf << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

pub fn base32_decode(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for ch in text.bytes() {
        let val = match ch {
            b'a'..=b'z' => ch - b'a',
            b'2'..=b'7' => ch - b'2' + 26,
            _ => return Err(CityError::malformed("invalid base32 character")),
        };
        buf = (buf << 5) | u32::from(val);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    if bits >= 5 || (bits > 0 && (buf & ((1 << bits) - 1)) != 0) {
        return Err(CityError::malformed("non-canonical base32 trailing bits"));
    }
    Ok(out)
}

/// A 36-byte DASL CIDv1 (`0x01 || codec || 0x12 || 0x20 || sha256`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cid([u8; 36]);

impl Cid {
    pub fn of(codec: u8, bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut raw = [0u8; 36];
        raw[0] = 0x01;
        raw[1] = codec;
        raw[2] = 0x12;
        raw[3] = 0x20;
        raw[4..].copy_from_slice(&digest);
        Self(raw)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 36
            || bytes[0] != 0x01
            || !matches!(bytes[1], CODEC_RAW | CODEC_DRISL)
            || bytes[2] != 0x12
            || bytes[3] != 0x20
        {
            return Err(CityError::malformed("not a DASL CIDv1"));
        }
        let mut raw = [0u8; 36];
        raw.copy_from_slice(bytes);
        Ok(Self(raw))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let rest = text
            .strip_prefix('b')
            .ok_or_else(|| CityError::malformed("CID must be multibase base32lower"))?;
        let bytes = base32_decode(rest)?;
        Self::from_bytes(&bytes)
    }

    pub fn codec(&self) -> u8 {
        self.0[1]
    }

    pub fn bytes(&self) -> &[u8; 36] {
        &self.0
    }
}

impl std::fmt::Display for Cid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("b")?;
        f.write_str(&base32_encode(&self.0))
    }
}

pub fn raw_cid_string(bytes: &[u8]) -> String {
    Cid::of(CODEC_RAW, bytes).to_string()
}

pub fn is_raw_cid(text: &str) -> bool {
    Cid::parse(text).is_ok_and(|cid| cid.codec() == CODEC_RAW)
}

// =========================================================================
// DRISL (Deterministic CBOR)
// =========================================================================

pub type ZuMap = BTreeMap<String, ZuValue>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZuValue {
    Null,
    Bool(bool),
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    List(Vec<ZuValue>),
    Map(ZuMap),
    Link(Cid),
}

impl ZuValue {
    pub fn as_map(&self) -> Option<&ZuMap> {
        match self {
            ZuValue::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ZuValue::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ZuValue::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ZuValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            ZuValue::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[ZuValue]> {
        match self {
            ZuValue::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_link(&self) -> Option<Cid> {
        match self {
            ZuValue::Link(cid) => Some(*cid),
            _ => None,
        }
    }
}

fn write_head(major: u8, n: u64, out: &mut Vec<u8>) {
    let m = major << 5;
    if n < 24 {
        out.push(m | (n as u8));
    } else if n <= 0xff {
        out.push(m | 24);
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(m | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= 0xffff_ffff {
        out.push(m | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

pub fn encode(value: &ZuValue) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, 0, &mut out);
    out
}

pub fn encode_map(map: &ZuMap) -> Vec<u8> {
    let mut out = Vec::new();
    encode_map_into(map, 0, &mut out);
    out
}

fn encode_into(value: &ZuValue, depth: usize, out: &mut Vec<u8>) {
    match value {
        ZuValue::Null => out.push(0xf6),
        ZuValue::Bool(false) => out.push(0xf4),
        ZuValue::Bool(true) => out.push(0xf5),
        ZuValue::Int(n) if *n >= 0 => write_head(0, *n as u64, out),
        ZuValue::Int(n) => write_head(1, (-1i128 - i128::from(*n)) as u64, out),
        ZuValue::Bytes(bytes) => {
            write_head(2, bytes.len() as u64, out);
            out.extend_from_slice(bytes);
        }
        ZuValue::Text(text) => {
            write_head(3, text.len() as u64, out);
            out.extend_from_slice(text.as_bytes());
        }
        ZuValue::List(items) => {
            write_head(4, items.len() as u64, out);
            for item in items {
                encode_into(item, depth + 1, out);
            }
        }
        ZuValue::Map(map) => encode_map_into(map, depth, out),
        ZuValue::Link(cid) => {
            // Tag 42 (0xd8 0x2a), byte string of 37 bytes: 0x00 identity multibase + 36-byte CID.
            out.extend_from_slice(&[0xd8, 0x2a, 0x58, 0x25, 0x00]);
            out.extend_from_slice(cid.bytes());
        }
    }
}

fn encode_map_into(map: &ZuMap, depth: usize, out: &mut Vec<u8>) {
    let mut entries: Vec<(&String, &ZuValue)> = map.iter().collect();
    entries.sort_by(|(a, _), (b, _)| {
        a.len()
            .cmp(&b.len())
            .then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });
    write_head(5, entries.len() as u64, out);
    for (key, value) in entries {
        write_head(3, key.len() as u64, out);
        out.extend_from_slice(key.as_bytes());
        encode_into(value, depth + 1, out);
    }
}

pub fn decode(bytes: &[u8]) -> Result<ZuValue> {
    let mut pos = 0usize;
    let value = decode_at(bytes, &mut pos, 0)?;
    if pos != bytes.len() {
        return Err(CityError::protocol("trailing bytes after DRISL value"));
    }
    Ok(value)
}

fn read_head(bytes: &[u8], pos: &mut usize) -> Result<(u8, u64)> {
    let first = *bytes
        .get(*pos)
        .ok_or_else(|| CityError::protocol("unexpected end of DRISL input"))?;
    *pos += 1;
    let major = first >> 5;
    let info = first & 0x1f;
    let arg = match info {
        0..=23 => u64::from(info),
        24 => {
            let v = *bytes
                .get(*pos)
                .ok_or_else(|| CityError::protocol("truncated 1-byte CBOR length"))?;
            *pos += 1;
            if v < 24 {
                return Err(CityError::protocol("non-canonical 1-byte CBOR integer"));
            }
            u64::from(v)
        }
        25 => {
            let slice = bytes
                .get(*pos..*pos + 2)
                .ok_or_else(|| CityError::protocol("truncated 2-byte CBOR length"))?;
            *pos += 2;
            let v = u16::from_be_bytes([slice[0], slice[1]]);
            if v <= 0xff {
                return Err(CityError::protocol("non-canonical 2-byte CBOR integer"));
            }
            u64::from(v)
        }
        26 => {
            let slice = bytes
                .get(*pos..*pos + 4)
                .ok_or_else(|| CityError::protocol("truncated 4-byte CBOR length"))?;
            *pos += 4;
            let v = u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
            if v <= 0xffff {
                return Err(CityError::protocol("non-canonical 4-byte CBOR integer"));
            }
            u64::from(v)
        }
        27 => {
            let slice = bytes
                .get(*pos..*pos + 8)
                .ok_or_else(|| CityError::protocol("truncated 8-byte CBOR length"))?;
            *pos += 8;
            let v = u64::from_be_bytes(slice.try_into().unwrap());
            if v <= 0xffff_ffff {
                return Err(CityError::protocol("non-canonical 8-byte CBOR integer"));
            }
            v
        }
        _ => return Err(CityError::protocol("indefinite or reserved CBOR head")),
    };
    Ok((major, arg))
}

fn decode_at(bytes: &[u8], pos: &mut usize, depth: usize) -> Result<ZuValue> {
    if depth > MAX_CBOR_DEPTH {
        return Err(CityError::protocol("DRISL nested too deeply"));
    }
    let (major, arg) = read_head(bytes, pos)?;
    match major {
        0 => {
            let n = i64::try_from(arg)
                .map_err(|_| CityError::protocol("unsigned integer exceeds i64"))?;
            Ok(ZuValue::Int(n))
        }
        1 => {
            if arg > i64::MAX as u64 {
                return Err(CityError::protocol("negative integer exceeds i64"));
            }
            let n = (-1i128 - i128::from(arg)) as i64;
            Ok(ZuValue::Int(n))
        }
        2 => {
            let len = usize::try_from(arg)
                .map_err(|_| CityError::protocol("byte string length overflow"))?;
            let slice = bytes
                .get(*pos..*pos + len)
                .ok_or_else(|| CityError::protocol("truncated CBOR byte string"))?;
            *pos += len;
            Ok(ZuValue::Bytes(slice.to_vec()))
        }
        3 => {
            let len = usize::try_from(arg)
                .map_err(|_| CityError::protocol("text string length overflow"))?;
            let slice = bytes
                .get(*pos..*pos + len)
                .ok_or_else(|| CityError::protocol("truncated CBOR text string"))?;
            *pos += len;
            let text = std::str::from_utf8(slice)
                .map_err(|_| CityError::protocol("invalid UTF-8 in CBOR text"))?;
            Ok(ZuValue::Text(text.to_string()))
        }
        4 => {
            let len =
                usize::try_from(arg).map_err(|_| CityError::protocol("array length overflow"))?;
            if len > 100_000 {
                return Err(CityError::protocol("CBOR array too large"));
            }
            let mut items = Vec::with_capacity(len.min(1024));
            for _ in 0..len {
                items.push(decode_at(bytes, pos, depth + 1)?);
            }
            Ok(ZuValue::List(items))
        }
        5 => {
            let len =
                usize::try_from(arg).map_err(|_| CityError::protocol("map length overflow"))?;
            if len > 100_000 {
                return Err(CityError::protocol("CBOR map too large"));
            }
            let mut map = ZuMap::new();
            let mut prev_key: Option<String> = None;
            for _ in 0..len {
                let (kmajor, karg) = read_head(bytes, pos)?;
                if kmajor != 3 {
                    return Err(CityError::protocol("DRISL map key must be text"));
                }
                let klen = usize::try_from(karg)
                    .map_err(|_| CityError::protocol("map key length overflow"))?;
                let kslice = bytes
                    .get(*pos..*pos + klen)
                    .ok_or_else(|| CityError::protocol("truncated map key"))?;
                *pos += klen;
                let key = std::str::from_utf8(kslice)
                    .map_err(|_| CityError::protocol("invalid UTF-8 in map key"))?
                    .to_string();
                if let Some(prev) = &prev_key {
                    let order = prev
                        .len()
                        .cmp(&key.len())
                        .then_with(|| prev.as_bytes().cmp(key.as_bytes()));
                    if !order.is_lt() {
                        return Err(CityError::protocol("DRISL map keys not strictly sorted"));
                    }
                }
                prev_key = Some(key.clone());
                let val = decode_at(bytes, pos, depth + 1)?;
                map.insert(key, val);
            }
            Ok(ZuValue::Map(map))
        }
        6 => {
            if arg != 42 {
                return Err(CityError::protocol("unsupported CBOR tag"));
            }
            let inner = decode_at(bytes, pos, depth + 1)?;
            let ZuValue::Bytes(raw) = inner else {
                return Err(CityError::protocol("tag 42 must wrap bytes"));
            };
            if raw.first() != Some(&0x00) {
                return Err(CityError::protocol(
                    "CID link missing 0x00 multibase prefix",
                ));
            }
            Ok(ZuValue::Link(Cid::from_bytes(&raw[1..])?))
        }
        7 => match arg {
            20 => Ok(ZuValue::Bool(false)),
            21 => Ok(ZuValue::Bool(true)),
            22 => Ok(ZuValue::Null),
            _ => Err(CityError::protocol("unsupported CBOR simple/float value")),
        },
        _ => Err(CityError::protocol("unknown CBOR major type")),
    }
}

pub fn blob_ref(cid: Cid, mime: &str, size: u64) -> ZuValue {
    let mut m = ZuMap::new();
    m.insert("$type".into(), ZuValue::Text("blob".into()));
    m.insert("mime".into(), ZuValue::Text(mime.into()));
    m.insert("ref".into(), ZuValue::Link(cid));
    m.insert("size".into(), ZuValue::Int(size as i64));
    ZuValue::Map(m)
}

/// Converts a `ZuValue` into `serde_json::Value` so existing reducers and debug
/// storage can inspect body fields uniformly.
pub fn zu_to_json(value: &ZuValue) -> Value {
    match value {
        ZuValue::Null => Value::Null,
        ZuValue::Bool(b) => Value::Bool(*b),
        ZuValue::Int(n) => json!(*n),
        ZuValue::Bytes(b) => json!({ "$bytes": b64url(b) }),
        ZuValue::Text(s) => Value::String(s.clone()),
        ZuValue::List(items) => Value::Array(items.iter().map(zu_to_json).collect()),
        ZuValue::Map(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), zu_to_json(v));
            }
            Value::Object(out)
        }
        ZuValue::Link(cid) => json!({ "$link": cid.to_string() }),
    }
}

pub fn json_to_zu(value: &Value) -> Result<ZuValue> {
    match value {
        Value::Null => Ok(ZuValue::Null),
        Value::Bool(b) => Ok(ZuValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(ZuValue::Int(i))
            } else if let Some(u) = n.as_u64() {
                let i = i64::try_from(u)
                    .map_err(|_| CityError::malformed("integer exceeds signed 64 bits"))?;
                Ok(ZuValue::Int(i))
            } else {
                Err(CityError::malformed("floats are not allowed in DRISL"))
            }
        }
        Value::String(s) => Ok(ZuValue::Text(s.clone())),
        Value::Array(items) => items
            .iter()
            .map(json_to_zu)
            .collect::<Result<Vec<_>>>()
            .map(ZuValue::List),
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(Value::String(cid_str)) = map.get("$link") {
                    return Ok(ZuValue::Link(Cid::parse(cid_str)?));
                }
                if let Some(Value::String(b64)) = map.get("$bytes") {
                    return Ok(ZuValue::Bytes(crate::text::unb64url(b64)?));
                }
            }
            let mut out = ZuMap::new();
            for (k, v) in map {
                out.insert(k.clone(), json_to_zu(v)?);
            }
            Ok(ZuValue::Map(out))
        }
    }
}

// =========================================================================
// Zu Device Login (`zu-login-1` + `pol-login-check-1`)
// =========================================================================

pub struct ZuLoginReady {
    pub did: String,
    pub seed: [u8; 32],
    pub digits: String,
}

impl Drop for ZuLoginReady {
    fn drop(&mut self) {
        self.seed.iter_mut().for_each(|b| *b = 0);
    }
}

pub struct ZuLoginSession {
    room: String,
    room_url: String,
    code: String,
    secret: SecretKey,
    public_bytes: [u8; 65],
    ready: Option<ZuLoginReady>,
}

fn random_p256_secret() -> SecretKey {
    loop {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::rng().fill_bytes(bytes.as_mut_slice());
        if let Ok(secret) = SecretKey::from_slice(bytes.as_slice()) {
            return secret;
        }
    }
}

impl ZuLoginSession {
    pub fn new(server_base: &str) -> Self {
        let mut room_bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut room_bytes);
        let room = hex::encode(room_bytes);
        let secret = random_p256_secret();
        let point = secret.public_key().to_sec1_point(false);
        let mut public_bytes = [0u8; 65];
        public_bytes.copy_from_slice(point.as_bytes());
        let base = server_base.trim_end_matches('/');
        let room_url = format!("{base}/.well-known/zu-relay/{room}");
        let code = format!("{room_url}#{}", b64url(&public_bytes));
        Self {
            room,
            room_url,
            code,
            secret,
            public_bytes,
            ready: None,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn room(&self) -> &str {
        &self.room
    }

    pub fn room_url(&self) -> &str {
        &self.room_url
    }

    pub fn ready(&self) -> Option<&ZuLoginReady> {
        self.ready.as_ref()
    }

    pub fn take_ready(&mut self) -> Option<ZuLoginReady> {
        self.ready.take()
    }

    /// Unseals the `sealed` DRISL envelope from `GET /.well-known/zu-relay/<room>`
    /// and computes the 6-digit check code (`pol-login-check-1`).
    pub fn unseal(&mut self, relay_bytes: &[u8]) -> Result<String> {
        if let Some(ready) = &self.ready {
            return Ok(ready.digits.clone());
        }
        let decoded = decode(relay_bytes)?;
        let map = decoded
            .as_map()
            .ok_or_else(|| CityError::protocol("relay payload is not a DRISL map"))?;
        if map.get("$type").and_then(ZuValue::as_str) != Some("sealed")
            || map.get("v").and_then(ZuValue::as_i64) != Some(1)
        {
            return Err(CityError::protocol("relay payload is not sealed v1"));
        }
        let epk_bytes = map
            .get("epk")
            .and_then(ZuValue::as_bytes)
            .filter(|b| b.len() == 65 && b[0] == 0x04)
            .ok_or_else(|| CityError::protocol("sealed epk is not 65-byte uncompressed P-256"))?;
        let iv = map
            .get("iv")
            .and_then(ZuValue::as_bytes)
            .filter(|b| b.len() == 12)
            .ok_or_else(|| CityError::protocol("sealed iv is not 12 bytes"))?;
        let ct = map
            .get("ct")
            .and_then(ZuValue::as_bytes)
            .filter(|b| b.len() >= 16)
            .ok_or_else(|| CityError::protocol("sealed ct is too short"))?;

        let peer = PublicKey::from_sec1_bytes(epk_bytes).map_err(|_| CityError::Crypto)?;
        let shared = diffie_hellman(self.secret.to_nonzero_scalar(), peer.as_affine());
        let mut salt = [0u8; 130];
        salt[..65].copy_from_slice(&self.public_bytes);
        salt[65..].copy_from_slice(epk_bytes);
        let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared.raw_secret_bytes().as_slice());
        let mut aes_key = [0u8; 32];
        hkdf.expand(LOGIN_INFO, &mut aes_key)
            .map_err(|_| CityError::Crypto)?;
        let cipher = Aes256Gcm::new_from_slice(&aes_key).map_err(|_| CityError::Crypto)?;
        aes_key.iter_mut().for_each(|b| *b = 0);

        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(iv),
                Payload {
                    msg: ct,
                    aad: self.room_url.as_bytes(),
                },
            )
            .map_err(|_| CityError::Crypto)?;

        let inner = decode(&plaintext)?;
        let inner_map = inner
            .as_map()
            .ok_or_else(|| CityError::protocol("inner login message is not a map"))?;
        if inner_map.get("$type").and_then(ZuValue::as_str) != Some("key")
            || inner_map.get("v").and_then(ZuValue::as_i64) != Some(1)
        {
            return Err(CityError::protocol("inner login message is not key v1"));
        }
        let did = inner_map
            .get("did")
            .and_then(ZuValue::as_str)
            .filter(|d| is_accepted_did(d))
            .ok_or_else(|| CityError::protocol("login message carries invalid DID"))?
            .to_string();
        let key_slice = inner_map
            .get("key")
            .and_then(ZuValue::as_bytes)
            .filter(|b| b.len() == 32)
            .ok_or_else(|| CityError::protocol("login message key is not 32 bytes"))?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(key_slice);

        let digits = login_check_digits(&self.public_bytes, epk_bytes);
        self.ready = Some(ZuLoginReady {
            did,
            seed,
            digits: digits.clone(),
        });
        Ok(digits)
    }
}

pub fn login_check_digits(code_public: &[u8; 65], epk: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(LOGIN_CHECK_DOMAIN);
    hasher.update(code_public);
    hasher.update(epk);
    let digest = hasher.finalize();
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1_000, n % 1_000)
}

/// Seals a `zu-login-1` key message (`{ $type: "key", v: 1, did, key }`) for a
/// receiver's QR code URL and 65-byte uncompressed P-256 public key, returning
/// the DRISL `sealed` bytes and the 6-digit check code (`pol-login-check-1`).
pub fn seal_login_key(
    room_url: &str,
    receiver_pub_bytes: &[u8; 65],
    did: &str,
    seed: &[u8; 32],
) -> Result<(Vec<u8>, String)> {
    let receiver_pub =
        PublicKey::from_sec1_bytes(receiver_pub_bytes).map_err(|_| CityError::Crypto)?;
    let eph = random_p256_secret();
    let epk_point = eph.public_key().to_sec1_point(false);
    let epk = epk_point.as_bytes();
    let shared = diffie_hellman(eph.to_nonzero_scalar(), receiver_pub.as_affine());
    let mut salt = [0u8; 130];
    salt[..65].copy_from_slice(receiver_pub_bytes);
    salt[65..].copy_from_slice(epk);
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared.raw_secret_bytes().as_slice());
    let mut aes_key = Zeroizing::new([0u8; 32]);
    hkdf.expand(LOGIN_INFO, aes_key.as_mut_slice())
        .map_err(|_| CityError::Crypto)?;
    let mut key_msg = ZuMap::new();
    key_msg.insert("$type".into(), ZuValue::Text("key".into()));
    key_msg.insert("v".into(), ZuValue::Int(1));
    key_msg.insert("did".into(), ZuValue::Text(did.to_string()));
    key_msg.insert("key".into(), ZuValue::Bytes(seed.to_vec()));
    let mut iv = [0u8; 12];
    rand::rng().fill_bytes(&mut iv);
    let cipher = Aes256Gcm::new_from_slice(aes_key.as_slice()).map_err(|_| CityError::Crypto)?;
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &encode_map(&key_msg),
                aad: room_url.as_bytes(),
            },
        )
        .map_err(|_| CityError::Crypto)?;
    let mut sealed = ZuMap::new();
    sealed.insert("$type".into(), ZuValue::Text("sealed".into()));
    sealed.insert("v".into(), ZuValue::Int(1));
    sealed.insert("epk".into(), ZuValue::Bytes(epk.to_vec()));
    sealed.insert("iv".into(), ZuValue::Bytes(iv.to_vec()));
    sealed.insert("ct".into(), ZuValue::Bytes(ct));
    let digits = login_check_digits(receiver_pub_bytes, epk);
    Ok((encode_map(&sealed), digits))
}

pub fn is_accepted_did(did: &str) -> bool {
    (did.starts_with("did:zu:") || did.starts_with("did:web:"))
        && did.len() <= 256
        && did
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:%-".contains(&b))
}

pub fn multikey_of_seed(seed: &[u8; 32]) -> String {
    let signing = SigningKey::from_bytes(seed);
    let public = signing.verifying_key().to_bytes();
    let mut prefixed = [0u8; 34];
    prefixed[0] = 0xed;
    prefixed[1] = 0x01;
    prefixed[2..].copy_from_slice(&public);
    format!("z{}", base58(&prefixed))
}

/// Checks whether a DID document lists `expected_multikey` under `#zu` or
/// `#zu-...`, and extracts claimed `zu://` handles.
pub fn verify_did_document(
    doc: &Value,
    expected_did: &str,
    expected_multikey: &str,
) -> Result<Vec<String>> {
    if doc.get("id").and_then(Value::as_str) != Some(expected_did) {
        return Err(CityError::Crypto);
    }
    let prefix = format!("{expected_did}#");
    let mut matched = false;
    if let Some(vms) = doc.get("verificationMethod").and_then(Value::as_array) {
        for vm in vms {
            if vm.get("type").and_then(Value::as_str) != Some("Multikey")
                || vm.get("controller").and_then(Value::as_str) != Some(expected_did)
            {
                continue;
            }
            let Some(id) = vm.get("id").and_then(Value::as_str) else {
                continue;
            };
            let fragment = id.strip_prefix('#').or_else(|| id.strip_prefix(&prefix));
            let Some(frag) = fragment else {
                continue;
            };
            let is_zu_frag = frag == "zu"
                || frag.strip_prefix("zu-").is_some_and(|rest| {
                    !rest.is_empty()
                        && rest
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                });
            if is_zu_frag
                && vm.get("publicKeyMultibase").and_then(Value::as_str) == Some(expected_multikey)
            {
                matched = true;
            }
        }
    }
    if !matched {
        return Err(CityError::Crypto);
    }
    let mut handles = Vec::new();
    if let Some(aka) = doc.get("alsoKnownAs").and_then(Value::as_array) {
        for entry in aka.iter().filter_map(Value::as_str) {
            if let Some(h) = entry.strip_prefix("zu://") {
                let clean = h.trim().to_ascii_lowercase();
                if !clean.is_empty() && !handles.contains(&clean) {
                    handles.push(clean);
                }
            }
        }
    }
    Ok(handles)
}

/// Checks an audit log response (`GET <directory>/<did>/log/audit`) when the
/// caller fetches the audit log directly.
pub fn verify_audit_log_current_key(
    entries: &[Value],
    expected_did: &str,
    expected_multikey: &str,
) -> Result<Vec<String>> {
    let mut active_op: Option<&Value> = None;
    for entry in entries {
        if entry.get("nullified").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if entry.get("did").and_then(Value::as_str) != Some(expected_did) {
            return Err(CityError::Crypto);
        }
        let Some(op) = entry.get("operation") else {
            return Err(CityError::Crypto);
        };
        if op.get("type").and_then(Value::as_str) == Some("plc_tombstone") {
            return Err(CityError::Crypto);
        }
        active_op = Some(op);
    }
    let op = active_op.ok_or(CityError::Crypto)?;
    let vms = op
        .get("verificationMethods")
        .and_then(Value::as_object)
        .ok_or(CityError::Crypto)?;
    let expected_did_key = format!("did:key:{expected_multikey}");
    let matched = vms.iter().any(|(name, val)| {
        let is_zu = name == "zu"
            || name.strip_prefix("zu-").is_some_and(|rest| {
                !rest.is_empty()
                    && rest
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            });
        is_zu && val.as_str() == Some(expected_did_key.as_str())
    });
    if !matched {
        return Err(CityError::Crypto);
    }
    let mut handles = Vec::new();
    if let Some(aka) = op.get("alsoKnownAs").and_then(Value::as_array) {
        for entry in aka.iter().filter_map(Value::as_str) {
            if let Some(h) = entry.strip_prefix("zu://") {
                let clean = h.trim().to_ascii_lowercase();
                if !clean.is_empty() && !handles.contains(&clean) {
                    handles.push(clean);
                }
            }
        }
    }
    Ok(handles)
}

// =========================================================================
// Signed Zu Records, Queries & Delivery
// =========================================================================

/// Wrapper key inside `serde_json::Value` so V0.3 signed records travel through
/// existing `publish` / `city_api` signatures without changing public FFI types.
pub const ZU_DELIVERY_MARKER: &str = "$zu_cbor";

#[derive(Debug, Clone)]
pub struct SignedZuRecord {
    pub cid: Cid,
    pub header: ZuMap,
    pub body: Option<(Cid, ZuMap)>,
}

#[allow(clippy::too_many_arguments)]
pub fn sign_zu_record(
    seed: &[u8; 32],
    by: &str,
    to: Option<&str>,
    record_type: &str,
    key: &str,
    at_ms: i64,
    prev: &[Cid],
    body: Option<ZuMap>,
) -> Result<SignedZuRecord> {
    if !is_accepted_did(by) {
        return Err(CityError::malformed("by is not an accepted DID"));
    }
    if let Some(target) = to {
        if !is_accepted_did(target) {
            return Err(CityError::malformed("to is not an accepted DID"));
        }
    }
    let signing = SigningKey::from_bytes(seed);
    let pub_multikey = multikey_of_seed(seed);

    let mut sorted_prev = prev.to_vec();
    sorted_prev.sort();
    sorted_prev.dedup();

    let body_pair = body.map(|b| {
        let bytes = encode_map(&b);
        (Cid::of(CODEC_DRISL, &bytes), b)
    });

    let mut header = ZuMap::new();
    header.insert("$type".into(), ZuValue::Text("record".into()));
    header.insert("v".into(), ZuValue::Int(1));
    header.insert("by".into(), ZuValue::Text(by.to_string()));
    if let Some(target) = to {
        header.insert("to".into(), ZuValue::Text(target.to_string()));
    }
    header.insert("type".into(), ZuValue::Text(record_type.to_string()));
    header.insert("key".into(), ZuValue::Text(key.to_string()));
    header.insert("at".into(), ZuValue::Int(at_ms));
    header.insert(
        "prev".into(),
        ZuValue::List(sorted_prev.into_iter().map(ZuValue::Link).collect()),
    );
    header.insert(
        "body".into(),
        match &body_pair {
            Some((cid, _)) => ZuValue::Link(*cid),
            None => ZuValue::Null,
        },
    );
    header.insert("pub".into(), ZuValue::Text(pub_multikey));

    let unsigned_bytes = encode_map(&header);
    let sig = ed::sign(&signing, &unsigned_bytes);
    header.insert("sig".into(), ZuValue::Bytes(sig.to_vec()));

    let header_bytes = encode_map(&header);
    let cid = Cid::of(CODEC_DRISL, &header_bytes);
    Ok(SignedZuRecord {
        cid,
        header,
        body: body_pair,
    })
}

pub fn encode_delivery(records: &[SignedZuRecord]) -> Vec<u8> {
    let mut recs = Vec::with_capacity(records.len());
    let mut blocks = ZuMap::new();
    for rec in records {
        recs.push(ZuValue::Map(rec.header.clone()));
        if let Some((body_cid, body_map)) = &rec.body {
            blocks.insert(body_cid.to_string(), ZuValue::Map(body_map.clone()));
        }
    }
    let mut root = ZuMap::new();
    root.insert("recs".into(), ZuValue::List(recs));
    if !blocks.is_empty() {
        root.insert("blocks".into(), ZuValue::Map(blocks));
    }
    encode_map(&root)
}

/// Decodes `{ stat: { "<cid>": "ok" | "deny" | ... } }` from `POST /.well-known/zu-recs`.
pub fn decode_delivery_stats(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let root = decode(bytes)?;
    let map = root
        .as_map()
        .ok_or_else(|| CityError::protocol("zu-recs delivery response is not a map"))?;
    let stat_map = map.get("stat").and_then(ZuValue::as_map).unwrap_or(map);
    let mut out = BTreeMap::new();
    for (cid, val) in stat_map {
        if let Some(status) = val.as_str() {
            out.insert(cid.clone(), status.to_string());
        }
    }
    Ok(out)
}

/// One decoded record from `GET /.well-known/zu-recs`.
#[derive(Debug, Clone)]
pub struct HeldZuRecord {
    pub cid: Cid,
    pub by: String,
    pub to: Option<String>,
    pub record_type: String,
    pub key: String,
    pub at: i64,
    pub prev: Vec<Cid>,
    pub body: Option<ZuMap>,
}

impl HeldZuRecord {
    pub fn ref_str(&self) -> String {
        format!("{}~{}", self.by, self.key)
    }
}

pub struct QueryPage {
    pub records: Vec<HeldZuRecord>,
    pub next: Option<String>,
}

pub fn decode_query_page(bytes: &[u8]) -> Result<QueryPage> {
    let root = decode(bytes)?;
    let map = root
        .as_map()
        .ok_or_else(|| CityError::protocol("zu-recs query answer is not a map"))?;
    let recs = map
        .get("recs")
        .and_then(ZuValue::as_list)
        .ok_or_else(|| CityError::protocol("zu-recs query answer missing recs"))?;
    let blocks = map.get("blocks").and_then(ZuValue::as_map);
    let next = map
        .get("next")
        .and_then(ZuValue::as_str)
        .map(str::to_string);

    let mut out = Vec::with_capacity(recs.len());
    for item in recs {
        let Some(h) = item.as_map() else {
            continue;
        };
        if h.get("$type").and_then(ZuValue::as_str) != Some("record")
            || h.get("v").and_then(ZuValue::as_i64) != Some(1)
        {
            continue;
        }
        let (Some(by), Some(record_type), Some(key), Some(at), Some(pub_mk), Some(sig)) = (
            h.get("by").and_then(ZuValue::as_str),
            h.get("type").and_then(ZuValue::as_str),
            h.get("key").and_then(ZuValue::as_str),
            h.get("at").and_then(ZuValue::as_i64),
            h.get("pub").and_then(ZuValue::as_str),
            h.get("sig").and_then(ZuValue::as_bytes),
        ) else {
            continue;
        };
        // Verify intrinsic Ed25519 signature over header without `sig`.
        let Ok(pub_bytes) = ed::multikey_bytes(pub_mk) else {
            continue;
        };
        let mut unsigned = h.clone();
        unsigned.remove("sig");
        if !ed::verify(&pub_bytes, &encode_map(&unsigned), sig) {
            continue;
        }
        let cid = Cid::of(CODEC_DRISL, &encode_map(h));
        let prev: Vec<Cid> = h
            .get("prev")
            .and_then(ZuValue::as_list)
            .map(|list| list.iter().filter_map(ZuValue::as_link).collect())
            .unwrap_or_default();
        let body = match h.get("body") {
            Some(ZuValue::Link(body_cid)) => blocks
                .and_then(|b| b.get(&body_cid.to_string()))
                .and_then(ZuValue::as_map)
                .cloned(),
            _ => None,
        };
        out.push(HeldZuRecord {
            cid,
            by: by.to_string(),
            to: h.get("to").and_then(ZuValue::as_str).map(str::to_string),
            record_type: record_type.to_string(),
            key: key.to_string(),
            at,
            prev,
            body,
        });
    }
    Ok(QueryPage { records: out, next })
}

// =========================================================================
// Pol V0.3 City Governance (`specs/city.md` / `pol/packages/zu/src/city.ts`)
// =========================================================================

#[derive(Debug, Clone)]
pub struct OfficeTerm {
    pub who: String,
    pub role: String,
    pub from: i64,
    pub until: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct ZuCityView {
    pub did: String,
    pub residents: HashSet<String>,
    pub roles: HashMap<String, BTreeSet<String>>,
    pub terms: Vec<OfficeTerm>,
}

impl ZuCityView {
    pub fn roles_of(&self, did: &str) -> Vec<String> {
        let mut list = Vec::new();
        if self.residents.contains(did) {
            list.push(ROLE_RESIDENT.to_string());
        }
        if let Some(held) = self.roles.get(did) {
            for role in [ROLE_COORDINATOR, ROLE_CURATOR, ROLE_INVITER] {
                if held.contains(role) {
                    list.push(role.to_string());
                }
            }
        }
        list
    }

    pub fn holds(&self, did: &str, role: &str) -> bool {
        self.roles.get(did).is_some_and(|held| held.contains(role))
    }

    pub fn reviewer_now(&self, did: &str) -> bool {
        self.holds(did, ROLE_COORDINATOR) || self.holds(did, ROLE_CURATOR)
    }

    pub fn reviewer_at(&self, did: &str, at: i64) -> bool {
        self.terms.iter().any(|t| {
            t.who == did
                && (t.role == ROLE_COORDINATOR || t.role == ROLE_CURATOR)
                && t.from <= at
                && t.until.is_none_or(|u| at < u)
        })
    }
}

struct Proposal {
    at: i64,
    n: usize,
    votes: HashSet<String>,
}

pub fn reduce_city_governance(
    city_did: &str,
    role_records: &[HeldZuRecord],
    invite_records: &[HeldZuRecord],
) -> ZuCityView {
    // Order governance records into the canonical chain (`orderChain`).
    let ids: HashSet<Cid> = role_records.iter().map(|r| r.cid).collect();
    let before: HashMap<Cid, Vec<Cid>> = role_records
        .iter()
        .map(|r| {
            (
                r.cid,
                r.prev.iter().copied().filter(|p| ids.contains(p)).collect(),
            )
        })
        .collect();
    let mut placed: HashSet<Cid> = HashSet::new();
    let mut chain: Vec<&HeldZuRecord> = Vec::new();
    let mut last: Option<Cid> = None;
    loop {
        let mut next: Vec<&HeldZuRecord> = role_records
            .iter()
            .filter(|r| {
                if placed.contains(&r.cid) {
                    return false;
                }
                let prev = &before[&r.cid];
                if !prev.iter().all(|p| placed.contains(p)) {
                    return false;
                }
                match last {
                    None => prev.is_empty(),
                    Some(l) => prev.contains(&l),
                }
            })
            .collect();
        if next.is_empty() {
            break;
        }
        next.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.cid.cmp(&b.cid)));
        let chosen = next[0];
        for fork in &next[1..] {
            placed.insert(fork.cid);
        }
        chain.push(chosen);
        last = Some(chosen.cid);
        placed.insert(chosen.cid);
    }

    let invites: Vec<(&HeldZuRecord, [u8; 32])> = invite_records
        .iter()
        .filter(|r| r.to.as_deref() == Some(city_did))
        .filter_map(|r| {
            let hash = r.body.as_ref()?.get("hash")?.as_bytes()?;
            let arr: [u8; 32] = hash.try_into().ok()?;
            Some((r, arr))
        })
        .collect();

    let mut residents: HashSet<String> = HashSet::new();
    let mut roles: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut proposals: HashMap<(String, bool), Proposal> = HashMap::new();
    let mut joins: HashMap<Cid, String> = HashMap::new();
    let mut used_invites: HashSet<Cid> = HashSet::new();

    let mut done_terms: Vec<OfficeTerm> = Vec::new();
    let mut open_terms: HashMap<(String, String), OfficeTerm> = HashMap::new();
    let mut latest_at = i64::MIN;

    let holds = |roles: &HashMap<String, BTreeSet<String>>, did: &str, role: &str| -> bool {
        roles.get(did).is_some_and(|s| s.contains(role))
    };
    let grant = |residents: &HashSet<String>,
                 roles: &mut HashMap<String, BTreeSet<String>>,
                 did: &str,
                 role: &str| {
        if residents.contains(did) {
            roles
                .entry(did.to_string())
                .or_default()
                .insert(role.to_string());
        }
    };
    let revoke = |roles: &mut HashMap<String, BTreeSet<String>>, did: &str, role: &str| {
        if let Some(set) = roles.get_mut(did) {
            set.remove(role);
        }
    };

    for rec in chain {
        if rec.to.as_deref() != Some(city_did) {
            continue;
        }
        if let Some(body) = &rec.body {
            let who = body.get("who").and_then(ZuValue::as_str).unwrap_or("");
            let kind = body.get("kind").and_then(ZuValue::as_str).unwrap_or("");
            let ok = body.get("ok").and_then(ZuValue::as_bool);
            let valid_kind = matches!(
                kind,
                ROLE_RESIDENT | ROLE_COORDINATOR | ROLE_CURATOR | ROLE_INVITER
            );
            if !who.is_empty() && valid_kind {
                if let Some(ok) = ok {
                    let prev_set: HashSet<Cid> = rec.prev.iter().copied().collect();
                    let lists_join = prev_set
                        .iter()
                        .any(|id| joins.get(id).map(String::as_str) == Some(who));

                    if rec.by == city_did {
                        // Rule 2: the city acts at once.
                        if kind == ROLE_RESIDENT {
                            if ok && lists_join {
                                residents.insert(who.to_string());
                            }
                        } else if ok {
                            grant(&residents, &mut roles, who, kind);
                        } else {
                            revoke(&mut roles, who, kind);
                        }
                    } else if kind == ROLE_RESIDENT {
                        // Rule 3: residents join with an admission, and leave on their own.
                        if rec.by == who && ok {
                            joins.insert(rec.cid, rec.by.clone());
                            if let Some(code) = body.get("code").and_then(ZuValue::as_bytes) {
                                let code_hash: [u8; 32] = Sha256::digest(code).into();
                                let matched = invites.iter().find(|(inv, h)| {
                                    prev_set.contains(&inv.cid)
                                        && !used_invites.contains(&inv.cid)
                                        && *h == code_hash
                                        && (holds(&roles, &inv.by, ROLE_INVITER)
                                            || holds(&roles, &inv.by, ROLE_COORDINATOR))
                                });
                                if let Some((inv, _)) = matched {
                                    used_invites.insert(inv.cid);
                                    residents.insert(rec.by.clone());
                                }
                            }
                        } else if rec.by == who {
                            residents.remove(&rec.by);
                            roles.remove(&rec.by);
                        } else if ok && holds(&roles, &rec.by, ROLE_COORDINATOR) && lists_join {
                            residents.insert(who.to_string());
                        }
                    } else if rec.by == who {
                        // Rule 6: anyone gives up a role they hold.
                        if !ok {
                            revoke(&mut roles, &rec.by, kind);
                        }
                    } else if holds(&roles, &rec.by, ROLE_COORDINATOR) {
                        if kind != ROLE_COORDINATOR {
                            // Rule 4: a coordinator appoints and dismisses curators and inviters at once.
                            if ok {
                                grant(&residents, &mut roles, who, kind);
                            } else {
                                revoke(&mut roles, who, kind);
                            }
                        } else {
                            // Rule 5: coordinators vote on another coordinator.
                            let pkey = (who.to_string(), ok);
                            if proposals
                                .get(&pkey)
                                .is_some_and(|p| rec.at.saturating_sub(p.at) > PROPOSAL_MS)
                            {
                                proposals.remove(&pkey);
                            }
                            let n_coords = roles
                                .values()
                                .filter(|s| s.contains(ROLE_COORDINATOR))
                                .count();
                            let entry = proposals.entry(pkey.clone()).or_insert_with(|| Proposal {
                                at: rec.at,
                                n: n_coords,
                                votes: HashSet::new(),
                            });
                            if entry.votes.insert(rec.by.clone()) && entry.votes.len() > entry.n / 2
                            {
                                if ok {
                                    grant(&residents, &mut roles, who, ROLE_COORDINATOR);
                                } else {
                                    revoke(&mut roles, who, ROLE_COORDINATOR);
                                }
                                proposals.remove(&pkey);
                            }
                        }
                    }
                }
            }
        }

        // Record office terms at `latest_at`.
        latest_at = latest_at.max(rec.at);
        let closed: Vec<(String, String)> = open_terms
            .keys()
            .filter(|(w, r)| !holds(&roles, w, r))
            .cloned()
            .collect();
        for key in closed {
            if let Some(mut term) = open_terms.remove(&key) {
                term.until = Some(latest_at);
                done_terms.push(term);
            }
        }
        for (w, held) in &roles {
            for r in held {
                let key = (w.clone(), r.clone());
                open_terms.entry(key).or_insert_with(|| OfficeTerm {
                    who: w.clone(),
                    role: r.clone(),
                    from: latest_at,
                    until: None,
                });
            }
        }
    }

    done_terms.extend(open_terms.into_values());
    done_terms.sort_by_key(|t| t.from);

    ZuCityView {
        did: city_did.to_string(),
        residents,
        roles,
        terms: done_terms,
    }
}

pub fn person_display_name(person_records: &[HeldZuRecord], handles: &[String]) -> String {
    // 1. `site/card` (`key == "card"` or newest) `body.name`
    if let Some(name) = person_records
        .iter()
        .filter(|r| r.record_type == "site/card")
        .max_by_key(|r| (r.key == "card", r.at))
        .and_then(|r| r.body.as_ref()?.get("name")?.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return name.to_string();
    }
    // 2. `garden/name` (`header.key` is the short name in the city)
    if let Some(name) = person_records
        .iter()
        .filter(|r| r.record_type == "garden/name" && r.body.is_some())
        .max_by_key(|r| r.at)
        .map(|r| r.key.trim())
        .filter(|s| !s.is_empty())
    {
        return name.to_string();
    }
    // 3. Primary handle from DID `alsoKnownAs` (`zu://alice.zuchiangmai.com` -> `alice`)
    if let Some(handle) = handles.first() {
        let short = handle.split('.').next().unwrap_or(handle).trim();
        if !short.is_empty() {
            return short.to_string();
        }
    }
    String::new()
}

// =========================================================================
// Pol V0.3 Calendar & Channels Reduction
// =========================================================================

const HOUR_MS: i64 = 3_600_000;

fn is_reading_body(body: &ZuMap) -> bool {
    body.get("v02")
        .and_then(ZuValue::as_map)
        .and_then(|v| v.get("by"))
        .and_then(ZuValue::as_str)
        .is_some()
}

/// Maps `did:v02:<64-hex>` stand-ins through `v02/link` records signed by the city.
fn build_v02_links(city_did: &str, v02_links: &[HeldZuRecord]) -> HashMap<String, String> {
    let mut best: HashMap<String, (i64, Cid, String)> = HashMap::new();
    for r in v02_links {
        if r.record_type != "v02/link" || r.by != city_did || r.to.as_deref() != Some(city_did) {
            continue;
        }
        let Some(body) = &r.body else { continue };
        if body.get("id").and_then(ZuValue::as_str) != Some(r.key.as_str()) {
            continue;
        }
        let Some(did) = body.get("did").and_then(ZuValue::as_str) else {
            continue;
        };
        if !did.starts_with("did:") || did.starts_with("did:v02:") {
            continue;
        }
        let replace = match best.get(&r.key) {
            None => true,
            Some((at, cid, _)) => (r.at, r.cid) > (*at, *cid),
        };
        if replace {
            best.insert(r.key.clone(), (r.at, r.cid, did.to_string()));
        }
    }
    best.into_iter().map(|(k, (_, _, did))| (k, did)).collect()
}

fn resolve_did(did: &str, links: &HashMap<String, String>) -> String {
    if let Some(old_id) = did.strip_prefix("did:v02:") {
        if let Some(mapped) = links.get(old_id) {
            return mapped.clone();
        }
    }
    did.to_string()
}

fn people_dids(
    value: Option<&ZuValue>,
    links: &HashMap<String, String>,
    max: usize,
) -> Vec<String> {
    let Some(items) = value.and_then(ZuValue::as_list) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items.iter().filter_map(ZuValue::as_str) {
        let resolved = resolve_did(item, links);
        if !resolved.is_empty() && !out.contains(&resolved) {
            out.push(resolved);
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Reduces Pol V0.3 `ev/*` records (plus `v02/link` stand-in mappings) into
/// `CityEvent` items on the city's calendar.
pub fn reduce_v03_events(
    city_view: &ZuCityView,
    ev_records: &[HeldZuRecord],
    v02_links: &[HeldZuRecord],
) -> Vec<CityEvent> {
    let city_did = &city_view.did;
    let links = build_v02_links(city_did, v02_links);

    // Track real `ev/event` records that supersede V0.2 archived entries (`body.v02.id`).
    let mut superseded_v02_ids: HashSet<String> = HashSet::new();
    for r in ev_records {
        if r.record_type == "ev/event"
            && !(r.by == *city_did && r.body.as_ref().is_some_and(is_reading_body))
        {
            if let Some(v02_id) = r
                .body
                .as_ref()
                .and_then(|b| b.get("v02"))
                .and_then(ZuValue::as_map)
                .and_then(|v| v.get("id"))
                .and_then(ZuValue::as_str)
            {
                superseded_v02_ids.insert(v02_id.to_string());
            }
        }
    }

    // Binned (`ev/trash`) by current reviewers.
    let mut trash_newest: HashMap<String, (i64, Cid, bool)> = HashMap::new();
    for r in ev_records.iter().filter(|r| r.record_type == "ev/trash") {
        if !city_view.reviewer_now(&r.by) {
            continue;
        }
        let Some(on) = r
            .body
            .as_ref()
            .and_then(|b| b.get("on"))
            .and_then(ZuValue::as_bool)
        else {
            continue;
        };
        let replace = match trash_newest.get(&r.key) {
            None => true,
            Some((at, cid, _)) => (r.at, r.cid) > (*at, *cid),
        };
        if replace {
            trash_newest.insert(r.key.clone(), (r.at, r.cid, on));
        }
    }
    let binned: HashSet<String> = trash_newest
        .into_iter()
        .filter_map(|(k, (_, _, on))| on.then_some(k))
        .collect();

    // Reviews (`ev/review`) by reviewers in office at signing time.
    let mut review_newest: HashMap<String, (i64, Cid, bool)> = HashMap::new();
    for r in ev_records.iter().filter(|r| r.record_type == "ev/review") {
        if !city_view.reviewer_at(&r.by, r.at) {
            continue;
        }
        let Some(ok) = r
            .body
            .as_ref()
            .and_then(|b| b.get("ok"))
            .and_then(ZuValue::as_bool)
        else {
            continue;
        };
        let replace = match review_newest.get(&r.key) {
            None => true,
            Some((at, cid, _)) => (r.at, r.cid) > (*at, *cid),
        };
        if replace {
            review_newest.insert(r.key.clone(), (r.at, r.cid, ok));
        }
    }

    // Host lists (`ev/hosts`) by first host or reviewer.
    type HostListEntry = (i64, Cid, String, Vec<String>);
    let mut host_lists: HashMap<String, Vec<HostListEntry>> = HashMap::new();
    for r in ev_records.iter().filter(|r| r.record_type == "ev/hosts") {
        if let Some(body) = &r.body {
            let list = people_dids(body.get("hosts"), &links, 100);
            host_lists.entry(r.key.clone()).or_default().push((
                r.at,
                r.cid,
                resolve_did(&r.by, &links),
                list,
            ));
        }
    }

    // Co-host edits (`ev/edit`).
    let mut edits_by_ref: HashMap<String, Vec<&HeldZuRecord>> = HashMap::new();
    for r in ev_records.iter().filter(|r| r.record_type == "ev/edit") {
        edits_by_ref.entry(r.key.clone()).or_default().push(r);
    }

    let mut out = Vec::new();
    for r in ev_records.iter().filter(|r| r.record_type == "ev/event") {
        let Some(body) = &r.body else { continue };
        let is_archived = r.by == *city_did && is_reading_body(body);
        let (event_ref, host_did, v02_listed) = if is_archived {
            let v02 = body.get("v02").and_then(ZuValue::as_map);
            let old_id = v02
                .and_then(|v| v.get("id"))
                .and_then(ZuValue::as_str)
                .unwrap_or("");
            if !old_id.is_empty() && superseded_v02_ids.contains(old_id) {
                continue;
            }
            let old_by = v02
                .and_then(|v| v.get("by"))
                .and_then(ZuValue::as_str)
                .unwrap_or("");
            let stand_in = format!("did:v02:{old_id_or_by}", old_id_or_by = old_by);
            let resolved_host = resolve_did(&stand_in, &links);
            let listed = v02
                .and_then(|v| v.get("listed"))
                .and_then(ZuValue::as_bool)
                .unwrap_or(true);
            // Archived entries keep their actual stored record ref (`<city_did>~<key>`)!
            // Wait: in `pol/packages/app-kit/src/kinds/v02.ts`, how is an archived entry's `ref` formed?
            // `by` becomes `did:v02:<v02.by>`, and `key` stays `r.key`, so `ref(item.by, item.key)` is `did:v02:<v02.by>~<r.key>`!
            (format!("{stand_in}~{}", r.key), resolved_host, listed)
        } else {
            (r.ref_str(), r.by.clone(), false)
        };

        if binned.contains(&event_ref) {
            continue;
        }

        let mut merged = body.clone();
        let mut hosts = vec![host_did.clone()];
        for m in people_dids(merged.get("mods"), &links, 8) {
            if !hosts.contains(&m) {
                hosts.push(m);
            }
        }
        if let Some(lists) = host_lists.get_mut(&event_ref) {
            lists.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
            for (_, _, signer, list) in lists.iter() {
                if signer == &host_did || city_view.reviewer_now(signer) {
                    hosts = vec![host_did.clone()];
                    for h in list {
                        if !hosts.contains(h) {
                            hosts.push(h.clone());
                        }
                    }
                }
            }
        }

        if let Some(edits) = edits_by_ref.get_mut(&event_ref) {
            edits.sort_by(|a, b| (a.at, a.cid).cmp(&(b.at, b.cid)));
            for edit in edits.iter() {
                let signer = resolve_did(&edit.by, &links);
                if hosts.contains(&signer)
                    || (r.by == *city_did && city_view.reviewer_at(&signer, edit.at))
                {
                    if let Some(ebody) = &edit.body {
                        for (k, v) in ebody {
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }

        let state = merged.get("state").and_then(ZuValue::as_str).unwrap_or("");
        if state == "removed" {
            continue;
        }
        let cancelled = merged.get("cancelled").and_then(ZuValue::as_bool) == Some(true)
            || state == "cancelled";
        let scope = merged
            .get("scope")
            .and_then(ZuValue::as_str)
            .filter(|s| ["public", "polis", "link"].contains(s))
            .unwrap_or("public")
            .to_string();

        // Check calendar standing (`standing === "on"` or host/crew).
        let approved = if is_archived {
            v02_listed
        } else if r.by == *city_did || city_view.reviewer_at(&host_did, r.at) {
            review_newest
                .get(&event_ref)
                .map(|(_, _, ok)| *ok)
                .unwrap_or(true)
        } else {
            review_newest
                .get(&event_ref)
                .map(|(_, _, ok)| *ok)
                .unwrap_or(false)
        };
        // Unlisted (`scope == "link"`) or pending events are still visible to their hosts/crew,
        // while declined events are excluded from the calendar.
        if let Some((_, _, false)) = review_newest.get(&event_ref) {
            continue;
        }

        let title = merged
            .get("title")
            .and_then(ZuValue::as_str)
            .map(|s| utf16_prefix(s.trim(), 120).to_string())
            .unwrap_or_default();
        let place = merged
            .get("place")
            .and_then(ZuValue::as_str)
            .map(|s| utf16_prefix(s.trim(), 200).to_string())
            .unwrap_or_default();
        let start_ms = merged
            .get("start")
            .and_then(ZuValue::as_i64)
            .unwrap_or(r.at);
        let end_ms = merged
            .get("end")
            .and_then(ZuValue::as_i64)
            .filter(|&end| end > start_ms)
            .unwrap_or(start_ms + 3 * HOUR_MS);
        let tz = merged
            .get("tz")
            .and_then(ZuValue::as_str)
            .unwrap_or("UTC")
            .to_string();
        let crew = people_dids(merged.get("crew"), &links, 8);
        let imported = r.by == *city_did && !is_archived && merged.contains_key("src");

        // Encode `approved` (`standing == "on"`) via `city`: empty when pending so
        // only hosts/crew/coordinators can file to a pending event, `city_did` when `"on"`.
        let on_city = if approved && scope != "link" {
            city_did.clone()
        } else {
            String::new()
        };

        out.push(CityEvent {
            id: event_ref,
            title,
            place,
            tz,
            scope,
            city: on_city,
            by: host_did,
            start_ms,
            end_ms,
            hosts,
            crew,
            cancelled,
            imported,
        });
    }
    out.sort_by(|a, b| (a.start_ms, &a.id).cmp(&(b.start_ms, &b.id)));
    out
}

pub fn reduce_v03_channels(city_view: &ZuCityView, chan_records: &[HeldZuRecord]) -> Vec<Channel> {
    let mut out = Vec::new();
    for r in chan_records
        .iter()
        .filter(|r| r.record_type == "arc/channel")
    {
        let Some(body) = &r.body else { continue };
        let set = body.get("set").and_then(ZuValue::as_bool) == Some(true);
        if set && !city_view.holds(&r.by, ROLE_COORDINATOR) {
            continue;
        }
        let title = body
            .get("title")
            .and_then(ZuValue::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let status = body
            .get("status")
            .and_then(ZuValue::as_str)
            .unwrap_or("open");
        let city = if set || city_view.residents.contains(&r.by) {
            Some(city_view.did.clone())
        } else {
            None
        };
        out.push(Channel {
            id: r.ref_str(),
            title,
            by: r.by.clone(),
            city,
            open: !set && status != "closed",
            set,
        });
    }
    out
}

// =========================================================================
// Pol V0.3 Recording & Archive Publishing (`tape/take` + `arc/item` + blobs)
// =========================================================================

#[derive(Debug, Clone)]
pub struct BuiltV03Filing {
    pub take_ref: String,
    pub block_ref: String,
    pub take_cid: Cid,
    pub take_key: String,
    pub take_body: ZuMap,
    pub delivery_bytes: Vec<u8>,
    pub blobs: Vec<(String, Vec<u8>)>,
}

/// Exact port of `newKey(now, salt)` in `pol/packages/app-kit/src/record.ts`.
pub fn new_record_key(now_ms: i64, salt: &str) -> String {
    let mut h = 0u32;
    for unit in salt.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u32::from(unit));
    }
    let time_b36 = radix_36_u64(now_ms.max(0) as u64);
    let hash_b36 = radix_36_u64(u64::from(h));
    let suffix: String = hash_b36.chars().take(4).collect();
    format!("{time_b36}-{suffix}")
}

fn radix_36_u64(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut chars = Vec::new();
    while n > 0 {
        chars.push(DIGITS[(n % 36) as usize] as char);
        n /= 36;
    }
    chars.iter().rev().collect()
}

fn mmss(ms: i64) -> String {
    let sec = (ms.max(0)) / 1_000;
    format!("{:02}:{:02}", sec / 60, sec % 60)
}

pub fn transcript_blob_bytes(sentences: &[Sentence]) -> Vec<u8> {
    let segs: Vec<Value> = sentences
        .iter()
        .filter_map(|s| {
            let text = utf16_prefix(js_trim(&s.text), SENTENCE_MAX);
            if text.is_empty() {
                return None;
            }
            Some(json!({
                "t": s.t_ms.max(0),
                "d": s.d_ms.max(0),
                "sp": crate::publish::speaker_label(&s.speaker),
                "text": text,
            }))
        })
        .collect();
    serde_json::to_vec(&json!({ "segs": segs })).unwrap_or_default()
}

pub fn translation_blob_bytes(lang: &str, items: &[Translated]) -> Vec<u8> {
    let segs: Vec<Value> = items
        .iter()
        .filter_map(|tr| {
            let text = utf16_prefix(js_trim(&tr.text), SENTENCE_MAX);
            if text.is_empty() {
                return None;
            }
            let of =
                crate::publish::text_mark(utf16_prefix(js_trim(&tr.source_text), SENTENCE_MAX));
            Some(json!({
                "t": tr.t_ms.max(0),
                "text": text,
                "of": of,
            }))
        })
        .collect();
    serde_json::to_vec(&json!({ "lang": lang, "segs": segs })).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
pub fn build_v03_take(
    seed: &[u8; 32],
    person_did: &str,
    city_did: &str,
    input: &TakeInput,
    at_ms: i64,
    existing_take_key: Option<&str>,
    prev_take_cid: Option<Cid>,
    existing_parts: Option<&ZuValue>,
) -> Result<BuiltV03Filing> {
    let take_key = existing_take_key
        .map(str::to_string)
        .unwrap_or_else(|| new_record_key(at_ms, &input.event_id));
    let title_trimmed = js_trim(&input.title);
    let title = utf16_prefix(
        if title_trimmed.is_empty() {
            "Recording"
        } else {
            title_trimmed
        },
        80,
    )
    .to_string();

    let mut blobs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut take_body = ZuMap::new();
    take_body.insert("event".into(), ZuValue::Text(input.event_id.clone()));
    take_body.insert("title".into(), ZuValue::Text(title.clone()));
    take_body.insert("start".into(), ZuValue::Int(input.started_ms.max(0)));
    take_body.insert(
        "end".into(),
        ZuValue::Int(input.started_ms.max(0) + input.duration_ms.max(0)),
    );
    take_body.insert("mime".into(), ZuValue::Text(MIME_M4A.to_string()));

    let speakers: Vec<ZuValue> = input
        .speakers
        .iter()
        .filter_map(|(sp, name)| {
            let clean_sp = crate::publish::speaker_label(sp);
            let clean_name = utf16_prefix(js_trim(name), 40);
            if clean_sp.is_empty() || clean_name.is_empty() {
                return None;
            }
            let mut m = ZuMap::new();
            m.insert("sp".into(), ZuValue::Text(clean_sp));
            m.insert("name".into(), ZuValue::Text(clean_name.to_string()));
            Some(ZuValue::Map(m))
        })
        .take(200)
        .collect();
    if !speakers.is_empty() {
        take_body.insert("speakers".into(), ZuValue::List(speakers));
    }

    if let Some(parts @ ZuValue::List(_)) = existing_parts {
        take_body.insert("parts".into(), parts.clone());
    }

    let text_bytes = transcript_blob_bytes(&input.sentences);
    if !input.sentences.is_empty() && !text_bytes.is_empty() {
        let text_cid = Cid::of(CODEC_RAW, &text_bytes);
        let size = text_bytes.len() as u64;
        let mut entry = ZuMap::new();
        entry.insert("file".into(), ZuValue::Text(text_cid.to_string()));
        entry.insert("size".into(), ZuValue::Int(size as i64));
        entry.insert("blob".into(), blob_ref(text_cid, "application/json", size));
        take_body.insert("text".into(), ZuValue::Map(entry));
        blobs.push((text_cid.to_string(), text_bytes));
    }

    let mut tr_entries = Vec::new();
    for (lang, items) in input.translations.iter().take(20) {
        if items.is_empty() {
            continue;
        }
        let tr_bytes = translation_blob_bytes(lang, items);
        let tr_cid = Cid::of(CODEC_RAW, &tr_bytes);
        let size = tr_bytes.len() as u64;
        let mut entry = ZuMap::new();
        entry.insert("lang".into(), ZuValue::Text(lang.clone()));
        entry.insert("file".into(), ZuValue::Text(tr_cid.to_string()));
        entry.insert("size".into(), ZuValue::Int(size as i64));
        entry.insert("blob".into(), blob_ref(tr_cid, "application/json", size));
        tr_entries.push(ZuValue::Map(entry));
        blobs.push((tr_cid.to_string(), tr_bytes));
    }
    if !tr_entries.is_empty() {
        take_body.insert("translations".into(), ZuValue::List(tr_entries));
    }

    let take_prev: Vec<Cid> = prev_take_cid.into_iter().collect();
    let signed_take = sign_zu_record(
        seed,
        person_did,
        Some(city_did),
        "tape/take",
        &take_key,
        at_ms,
        &take_prev,
        Some(take_body.clone()),
    )?;

    let mut to_deliver = vec![signed_take.clone()];
    let block_key = format!("tape-{take_key}");
    let block_ref = format!("{person_did}~{block_key}");

    if prev_take_cid.is_none() {
        let mut note_body = ZuMap::new();
        note_body.insert("kind".into(), ZuValue::Text("note".into()));
        let note_title = utf16_prefix(&format!("录音 · {title}"), 120).to_string();
        note_body.insert("title".into(), ZuValue::Text(note_title));
        note_body.insert(
            "text".into(),
            ZuValue::Text(format!("时长 {}", mmss(input.duration_ms))),
        );
        note_body.insert("event".into(), ZuValue::Text(input.event_id.clone()));
        note_body.insert(
            "tags".into(),
            ZuValue::List(vec![ZuValue::Text("录制".into())]),
        );
        let signed_note = sign_zu_record(
            seed,
            person_did,
            Some(city_did),
            "arc/item",
            &block_key,
            at_ms + 1,
            &[],
            Some(note_body),
        )?;
        to_deliver.push(signed_note);

        for (idx, chan_ref) in input.channel_ids.iter().enumerate() {
            let mut conn_body = ZuMap::new();
            conn_body.insert("chan".into(), ZuValue::Text(chan_ref.clone()));
            conn_body.insert("to".into(), ZuValue::Text(block_ref.clone()));
            let conn_key_hash =
                hex::encode(&Sha256::digest(format!("{chan_ref}\0{block_ref}").as_bytes())[..12]);
            let signed_conn = sign_zu_record(
                seed,
                person_did,
                Some(city_did),
                "arc/connect",
                &conn_key_hash,
                at_ms + 2 + idx as i64,
                &[],
                Some(conn_body),
            )?;
            to_deliver.push(signed_conn);
        }
    }

    let delivery_bytes = encode_delivery(&to_deliver);
    Ok(BuiltV03Filing {
        take_ref: format!("{person_did}~{take_key}"),
        block_ref,
        take_cid: signed_take.cid,
        take_key,
        take_body,
        delivery_bytes,
        blobs,
    })
}

/// Builds an updated `tape/take` Zu record adding or replacing audio part `n`.
#[allow(clippy::too_many_arguments)]
pub fn build_v03_part(
    seed: &[u8; 32],
    person_did: &str,
    city_did: &str,
    take_key: &str,
    prev_take_cid: Cid,
    prev_body: &ZuMap,
    input: &PartInput,
    at_ms: i64,
) -> Result<(Cid, ZuMap, Vec<u8>)> {
    if input.size == 0 || input.size > crate::hub::FILE_MAX_BYTES {
        return Err(if input.size == 0 {
            CityError::malformed("an audio part has to have bytes in it")
        } else {
            CityError::FileTooBig
        });
    }
    if input.t0_ms < 0 || input.dur_ms <= 0 {
        return Err(CityError::malformed(
            "part times must be non-negative and positive",
        ));
    }
    if input.levels.len() > 3_600 || !input.levels.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CityError::malformed(
            "levels must be up to 3600 ASCII digits",
        ));
    }
    let part_cid = Cid::parse(&input.file)?;
    if part_cid.codec() != CODEC_RAW {
        return Err(CityError::malformed("audio part file must be a raw CID"));
    }

    let mut entry = ZuMap::new();
    entry.insert("n".into(), ZuValue::Int(i64::from(input.n)));
    entry.insert("file".into(), ZuValue::Text(input.file.clone()));
    entry.insert("size".into(), ZuValue::Int(input.size as i64));
    entry.insert("t0".into(), ZuValue::Int(input.t0_ms));
    entry.insert("dur".into(), ZuValue::Int(input.dur_ms));
    entry.insert("mime".into(), ZuValue::Text(input.mime.clone()));
    entry.insert("lv".into(), ZuValue::Text(input.levels.clone()));
    entry.insert("blob".into(), blob_ref(part_cid, &input.mime, input.size));

    let mut parts: Vec<ZuValue> = prev_body
        .get("parts")
        .and_then(ZuValue::as_list)
        .map(|list| {
            list.iter()
                .filter(|item| {
                    item.as_map()
                        .and_then(|m| m.get("n"))
                        .and_then(ZuValue::as_i64)
                        != Some(i64::from(input.n))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    parts.push(ZuValue::Map(entry));
    parts.sort_by_key(|item| {
        item.as_map()
            .and_then(|m| m.get("n"))
            .and_then(ZuValue::as_i64)
            .unwrap_or(0)
    });

    let mut next_body = prev_body.clone();
    next_body.insert("parts".into(), ZuValue::List(parts));

    let signed = sign_zu_record(
        seed,
        person_did,
        Some(city_did),
        "tape/take",
        take_key,
        at_ms,
        &[prev_take_cid],
        Some(next_body.clone()),
    )?;
    let delivery = encode_delivery(std::slice::from_ref(&signed));
    Ok((signed.cid, next_body, delivery))
}

pub fn build_v03_withdraw(
    seed: &[u8; 32],
    person_did: &str,
    city_did: &str,
    block_ref: &str,
    at_ms: i64,
) -> Result<Vec<u8>> {
    let mut body = ZuMap::new();
    body.insert("to".into(), ZuValue::Text("drop".into()));
    let signed = sign_zu_record(
        seed,
        person_did,
        Some(city_did),
        "arc/mark",
        block_ref,
        at_ms,
        &[],
        Some(body),
    )?;
    Ok(encode_delivery(&[signed]))
}

/// Packs V0.3 `tape/take` state into `PartInput::take_id` for `publish::build_part`.
pub fn pack_v03_part_target(
    city_did: &str,
    take_key: &str,
    prev_cid: &str,
    prev_body_b64: &str,
) -> String {
    format!("zu-v03\n{city_did}\n{take_key}\n{prev_cid}\n{prev_body_b64}")
}

pub fn unpack_v03_part_target(packed: &str) -> Result<(String, String, Cid, ZuMap)> {
    let mut parts = packed.splitn(5, '\n');
    if parts.next() != Some("zu-v03") {
        return Err(CityError::malformed("not a packed V0.3 take target"));
    }
    let city_did = parts
        .next()
        .filter(|s| is_accepted_did(s))
        .ok_or_else(|| CityError::malformed("invalid V0.3 city DID"))?
        .to_string();
    let take_key = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CityError::malformed("empty V0.3 take key"))?
        .to_string();
    let prev_cid = Cid::parse(
        parts
            .next()
            .ok_or_else(|| CityError::malformed("missing V0.3 prev CID"))?,
    )?;
    let body_bytes = crate::text::unb64url(
        parts
            .next()
            .ok_or_else(|| CityError::malformed("missing V0.3 body"))?,
    )?;
    let prev_body = decode(&body_bytes)?
        .as_map()
        .cloned()
        .ok_or_else(|| CityError::malformed("V0.3 take body is not a map"))?;
    Ok((city_did, take_key, prev_cid, prev_body))
}

/// SHA-256 hex of a Pol V0.3 record ref (`<did>~<key>`), matching `fold.ts`'s
/// `idOf(ref)` used in `/archive/c/<chanId>/b/<blockId>` URLs.
pub fn archive_url_id(record_ref: &str) -> String {
    hex::encode(Sha256::digest(record_ref.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drisl_and_cid_round_trip() {
        let raw = Cid::of(CODEC_RAW, b"hello world");
        assert!(raw.to_string().starts_with("bafkrei"));
        assert_eq!(Cid::parse(&raw.to_string()).unwrap(), raw);

        let mut map = ZuMap::new();
        map.insert("bb".into(), ZuValue::Int(-42));
        map.insert("a".into(), ZuValue::Link(raw));
        map.insert("ccc".into(), ZuValue::Bool(true));
        let bytes = encode_map(&map);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, ZuValue::Map(map));
    }

    #[test]
    fn zu_login_unseals_and_matches_pol_six_digit_check() {
        let mut session = ZuLoginSession::new("https://zuchiangmai.com");
        let code = session.code().to_string();
        let (room_url, frag) = code.split_once('#').unwrap();
        assert_eq!(room_url, session.room_url());
        let receiver_pub_bytes = crate::text::unb64url_strict(frag).unwrap();
        let receiver_pub = PublicKey::from_sec1_bytes(&receiver_pub_bytes).unwrap();

        // Simulate phone (`sealKey` in `pol/packages/zu/src/login.ts` + `loginCheckDigits` in `link-check.ts`).
        let phone_eph = random_p256_secret();
        let phone_epk_point = phone_eph.public_key().to_sec1_point(false);
        let phone_epk = phone_epk_point.as_bytes();
        let shared = diffie_hellman(phone_eph.to_nonzero_scalar(), receiver_pub.as_affine());
        let mut salt = Vec::with_capacity(130);
        salt.extend_from_slice(&receiver_pub_bytes);
        salt.extend_from_slice(phone_epk);
        let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared.raw_secret_bytes().as_slice());
        let mut aes_key = [0u8; 32];
        hkdf.expand(b"zu-login-1", &mut aes_key).unwrap();

        let seed = [7u8; 32];
        let mut key_msg = ZuMap::new();
        key_msg.insert("$type".into(), ZuValue::Text("key".into()));
        key_msg.insert("v".into(), ZuValue::Int(1));
        key_msg.insert(
            "did".into(),
            ZuValue::Text("did:zu:ewvi7nxzyoun6zhxrhs64oiz".into()),
        );
        key_msg.insert("key".into(), ZuValue::Bytes(seed.to_vec()));
        let iv = [9u8; 12];
        let cipher = Aes256Gcm::new_from_slice(&aes_key).unwrap();
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: &encode_map(&key_msg),
                    aad: room_url.as_bytes(),
                },
            )
            .unwrap();

        let mut sealed = ZuMap::new();
        sealed.insert("$type".into(), ZuValue::Text("sealed".into()));
        sealed.insert("v".into(), ZuValue::Int(1));
        sealed.insert("epk".into(), ZuValue::Bytes(phone_epk.to_vec()));
        sealed.insert("iv".into(), ZuValue::Bytes(iv.to_vec()));
        sealed.insert("ct".into(), ZuValue::Bytes(ct));

        let digits = session.unseal(&encode_map(&sealed)).unwrap();
        assert_eq!(digits.len(), 7);
        assert_eq!(&digits[3..4], " ");
        let expected_digits = login_check_digits(&session.public_bytes, phone_epk);
        assert_eq!(digits, expected_digits);

        let ready = session.take_ready().unwrap();
        assert_eq!(ready.did, "did:zu:ewvi7nxzyoun6zhxrhs64oiz");
        assert_eq!(ready.seed, seed);
    }

    #[test]
    fn signed_zu_record_round_trips_through_query_decoder() {
        let seed = [11u8; 32];
        let did = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
        let city = "did:zu:city00000000000000000000";
        let mut body = ZuMap::new();
        body.insert("title".into(), ZuValue::Text("Opening Circle".into()));
        body.insert("start".into(), ZuValue::Int(1_795_000_000_000));
        body.insert("end".into(), ZuValue::Int(1_795_003_600_000));
        body.insert("place".into(), ZuValue::Text("Main Hall".into()));
        body.insert("about".into(), ZuValue::Text("Welcome".into()));
        let signed = sign_zu_record(
            &seed,
            did,
            Some(city),
            "ev/event",
            "k1",
            1_795_000_000_000,
            &[],
            Some(body),
        )
        .unwrap();
        let page_bytes = encode_delivery(std::slice::from_ref(&signed));
        let page = decode_query_page(&page_bytes).unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].cid, signed.cid);
        assert_eq!(page.records[0].ref_str(), format!("{did}~k1"));
    }

    #[test]
    fn governance_and_calendar_fold_v03_and_v02_archived_events() {
        let city_seed = [1u8; 32];
        let coord_seed = [2u8; 32];
        let res_seed = [3u8; 32];
        let city = "did:zu:city00000000000000000000";
        let coord = "did:zu:coord0000000000000000000";
        let resident = "did:zu:res000000000000000000000";

        let mk_role = |seed: &[u8; 32],
                       by: &str,
                       key: &str,
                       at: i64,
                       prev: &[Cid],
                       who: &str,
                       kind: &str,
                       ok: bool| {
            let mut b = ZuMap::new();
            b.insert("who".into(), ZuValue::Text(who.into()));
            b.insert("kind".into(), ZuValue::Text(kind.into()));
            b.insert("ok".into(), ZuValue::Bool(ok));
            let s =
                sign_zu_record(seed, by, Some(city), "city/role", key, at, prev, Some(b)).unwrap();
            HeldZuRecord {
                cid: s.cid,
                by: by.into(),
                to: Some(city.into()),
                record_type: "city/role".into(),
                key: key.into(),
                at,
                prev: prev.to_vec(),
                body: s.body.map(|(_, m)| m),
            }
        };

        let r1 = mk_role(
            &coord_seed,
            coord,
            "join1",
            10,
            &[],
            coord,
            "resident",
            true,
        );
        let r2 = mk_role(
            &city_seed,
            city,
            "admit1",
            20,
            &[r1.cid],
            coord,
            "resident",
            true,
        );
        let r3 = mk_role(
            &city_seed,
            city,
            "appoint1",
            30,
            &[r2.cid],
            coord,
            "coordinator",
            true,
        );
        let r4 = mk_role(
            &res_seed,
            resident,
            "join2",
            40,
            &[r3.cid],
            resident,
            "resident",
            true,
        );
        let r5 = mk_role(
            &coord_seed,
            coord,
            "admit2",
            50,
            &[r4.cid],
            resident,
            "resident",
            true,
        );

        let view = reduce_city_governance(city, &[r1, r2, r3, r4, r5], &[]);
        assert_eq!(view.roles_of(coord), vec!["resident", "coordinator"]);
        assert_eq!(view.roles_of(resident), vec!["resident"]);

        // Coordinator event is immediately on calendar (`city == city_did`).
        let mut ev_body = ZuMap::new();
        ev_body.insert("title".into(), ZuValue::Text("Town Hall".into()));
        ev_body.insert("start".into(), ZuValue::Int(1_000));
        ev_body.insert("end".into(), ZuValue::Int(5_000));
        ev_body.insert("place".into(), ZuValue::Text("Plaza".into()));
        ev_body.insert("about".into(), ZuValue::Text("Weekly".into()));
        let ev_signed = sign_zu_record(
            &coord_seed,
            coord,
            Some(city),
            "ev/event",
            "th1",
            100,
            &[],
            Some(ev_body),
        )
        .unwrap();
        let ev_held = HeldZuRecord {
            cid: ev_signed.cid,
            by: coord.into(),
            to: Some(city.into()),
            record_type: "ev/event".into(),
            key: "th1".into(),
            at: 100,
            prev: vec![],
            body: ev_signed.body.map(|(_, m)| m),
        };
        let events = reduce_v03_events(&view, &[ev_held], &[]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, format!("{coord}~th1"));
        assert_eq!(events[0].city, city);
    }
}
