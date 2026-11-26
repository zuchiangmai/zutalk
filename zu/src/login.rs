//! login.md: how a Zu signing key that one page holds reaches another page, or another device.
//!
//! A private key is its 32-byte seed (RFC 8032 § 5.1.5). Here a seed is never printed, and
//! what holds one is wiped when dropped. Randomness is a parameter, made anew for each login.

use crate::did::{Doc, base64url, is_did};
use crate::key::{Map, take};
use crate::{Error, SecretKey, Value, rule};
use aes_gcm::{Aes256Gcm, aead::AeadInOut, aead::KeyInit};
use multibase::Base;
use p256::{PublicKey as Point, SecretKey as Scalar, elliptic_curve::sec1::ToSec1Point};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

/// § Messages: a map that states its `$type` and its `v`. It is not signed.
fn message<'a>(kind: &str, fields: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    let stated = [("$type", Value::Str(kind.into())), ("v", Value::Int(1))];
    Value::Map(stated.into_iter().chain(fields).map(|(name, value)| (name.into(), value)).collect())
}

/// § Messages: the other fields of the message of this `$type`, from its DRISL bytes.
fn fields(bytes: &[u8], kind: &str) -> Result<Map, Error> {
    let Value::Map(mut map) = Value::decode(bytes)? else { return Err(Error::Field("$type")) };
    take!(map, "$type", Value::Str(stated) if stated == kind => ());
    take!(map, "v", Value::Int(1) => ());
    Ok(map)
}

/// § Finding the Login Page: what to `GET` at the origin of the handle the person gave.
pub fn finder(origin: &str) -> String {
    format!("{origin}/.well-known/zu-login")
}

/// § Finding the Login Page: the body of the answer, which is the `https` URL of the login
/// page as plain text with no wrapper.
pub fn page(body: &str) -> Result<&str, Error> {
    let host = body.strip_prefix("https://").unwrap_or_default();
    let named = host.starts_with(|c: char| c.is_ascii_alphanumeric());
    rule(named && body.bytes().all(|b| b.is_ascii_graphic()), Error::Url).map(|()| body)
}

/// § Messages: the request that the receiving page sends to the login page on the same
/// device. It names the DID the person wants to use, when the receiving page knows it.
pub fn request(did: Option<&str>) -> Vec<u8> {
    message("login", did.map(|did| ("did", Value::Str(did.into())))).encode()
}

/// § On the Same Device: the DID that a request names, if it names one. For the login page,
/// which takes the receiving origin from the browser and from no message.
pub fn requested(request: &[u8]) -> Result<Option<String>, Error> {
    match fields(request, "login")?.remove("did") {
        Some(Value::Str(did)) if is_did(&did) => Ok(Some(did)),
        None => Ok(None),
        Some(_) => Err(Error::Field("did")),
    }
}

/// § Messages: the key message, in the clear, with the private Zu signing key `seed` of `did`.
/// The login page makes it only once the person has agreed in that page. On the same device it
/// goes to the receiving origin only; between devices it is sealed first ([`Scanned::seal`]).
pub fn key(did: &str, seed: &[u8; 32]) -> Zeroizing<Vec<u8>> {
    // Encoded around 32 zeros, so that no value is left holding a copy of the seed. DRISL puts
    // the longest name last: `"$type": "key"` ends the message, and `key` is just before it.
    let blank = [("did", Value::Str(did.into())), ("key", Value::Bytes(vec![0; 32]))];
    let mut bytes = Zeroizing::new(message("key", blank).encode());
    let end = bytes.len() - 10;
    bytes[end - 32..end].copy_from_slice(seed);
    bytes
}

/// A key message that was received. Its key is handed out by [`Received::seed`] alone.
pub struct Received {
    /// The DID the key is said to belong to. Resolve its document, then ask for the key.
    pub did: String,
    seed: Zeroizing<[u8; 32]>,
}

/// § Receiving the Key: read a key message, however it arrived. `named` is the DID the person
/// named, if the person named one.
pub fn receive(message: &[u8], named: Option<&str>) -> Result<Received, Error> {
    let mut map = fields(message, "key")?;
    let did = take!(map, "did", Value::Str(did) if is_did(&did) => did);
    let mut key = take!(map, "key", Value::Bytes(key) => key);
    let seed = <[u8; 32]>::try_from(key.as_slice()).map(Zeroizing::new);
    key.zeroize();
    rule(named.is_none_or(|named| named == did), Error::Field("did"))?;
    Ok(Received { did, seed: seed.or(Err(Error::Field("key")))? })
}

impl Received {
    /// § Receiving the Key: the private key, if its public key is a Zu signing key in the
    /// current document of the DID, to store in a form that can be exported. If not, discard.
    pub fn seed(&self, doc: &Doc) -> Result<&[u8; 32], Error> {
        let key = SecretKey::from_seed(&self.seed).public();
        rule(doc.did == self.did && doc.key_now(&key), Error::Signer).map(|()| &*self.seed)
    }
}

/// A P-256 key pair from 32 random bytes: the private key, and the public key as 65 bytes.
/// About one draw in 2^32 is no private key: it is refused, and the caller makes another.
fn pair(random: &[u8; 32]) -> Result<(Scalar, Vec<u8>), Error> {
    let secret = Scalar::from_slice(random).or(Err(Error::Key))?;
    let public = secret.public_key().to_sec1_point(false).as_bytes().to_vec();
    Ok((secret, public))
}

/// A P-256 public key, from the 65 bytes of an uncompressed point.
fn point(bytes: &[u8]) -> Option<Point> {
    Point::from_sec1_bytes(bytes).ok().filter(|_| bytes.len() == 65)
}

/// § Sealing the Key: the AES-256-GCM key that both sides derive with HKDF-SHA-256 from the
/// ECDH shared secret. The salt is the public key in the code, then the login page's.
fn cipher(ours: &Scalar, theirs: &[u8], code: &[u8], epk: &[u8]) -> Result<Aes256Gcm, Error> {
    let shared = ours.diffie_hellman(&point(theirs).ok_or(Error::Key)?);
    let (mut key, salt) = (Zeroizing::new([0; 32]), [code, epk].concat());
    let derived = shared.extract::<Sha256>(Some(&salt)).expand(b"zu-login-1", &mut *key);
    derived.map(|()| Aes256Gcm::new((&*key).into())).or(Err(Error::Sealed))
}

/// § The Code, as the login page reads it. The page shows the person who receives the key,
/// the receiving `origin` and the name of its city, and waits for the person's choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    pub origin: String,
    /// The room URL: the code without its fragment. The sealed message goes there.
    pub room: String,
    key: Vec<u8>,
}

/// § The Code: read the content of a code, which the person scanned from within the login
/// page: an `https` origin, a room number, and in the fragment a P-256 public key.
pub fn scan(code: &str) -> Result<Scanned, Error> {
    let (room, key) = code.split_once('#').ok_or(Error::Url)?;
    let (origin, number) = room.split_once("/.well-known/zu-relay/").ok_or(Error::Url)?;
    let host = origin.strip_prefix("https://").unwrap_or_default();
    let named = host.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b));
    let hex = number.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    rule(!host.is_empty() && named && number.len() == 32 && hex, Error::Url)?;
    let key = base64url(key).filter(|key| point(key).is_some()).ok_or(Error::Url)?;
    Ok(Scanned { origin: origin.into(), room: room.into(), key })
}

impl Scanned {
    /// § Sealing the Key, once the person has agreed: the DRISL bytes of the sealed message
    /// that carries the key message `key`, to `POST` to `room` as `application/octet-stream`.
    /// `random` are 32 random bytes and `iv` 12, made for this one login.
    pub fn seal(&self, key: &[u8], random: &[u8; 32], iv: &[u8; 12]) -> Result<Vec<u8>, Error> {
        let ((secret, epk), mut text) = (pair(random)?, Zeroizing::new(key.to_vec()));
        let cipher = cipher(&secret, &self.key, &self.key, &epk)?;
        let sealed = cipher.encrypt_in_place(&(*iv).into(), self.room.as_bytes(), &mut *text);
        sealed.or(Err(Error::Sealed))?;
        let ct = Value::Bytes(std::mem::take(&mut *text));
        let fields = [("epk", Value::Bytes(epk)), ("iv", Value::Bytes(iv.into())), ("ct", ct)];
        Ok(message("sealed", fields).encode())
    }
}

/// § The Code: what the receiving page keeps for one login between devices.
pub struct Code {
    /// The content of the code to show, usually as a QR code.
    pub url: String,
    /// The room URL. The receiving page asks it (`GET`) until the sealed message is there.
    pub room: String,
    secret: Scalar,
    public: Vec<u8>,
}

impl Code {
    /// § The Code. `origin` is the receiving origin, whose host relays. `random` are 32 random
    /// bytes and `room` 16, made for this one login.
    pub fn new(origin: &str, random: &[u8; 32], room: &[u8; 16]) -> Result<Code, Error> {
        let (secret, public) = pair(random)?;
        let number: String = room.iter().map(|byte| format!("{byte:02x}")).collect();
        let key = Base::Base64Url.encode(&public);
        let url = format!("{origin}/.well-known/zu-relay/{number}#{key}");
        Ok(Code { room: scan(&url)?.room, url, secret, public })
    }

    /// § The Relay, § Receiving the Key: open the sealed message fetched from the room.
    pub fn open(&self, sealed: &[u8], named: Option<&str>) -> Result<Received, Error> {
        let mut map = fields(sealed, "sealed")?;
        let epk = take!(map, "epk", Value::Bytes(epk) => epk);
        let iv = take!(map, "iv", Value::Bytes(iv) => iv);
        let mut text = Zeroizing::new(take!(map, "ct", Value::Bytes(ct) => ct));
        let cipher = cipher(&self.secret, &epk, &self.public, &epk).or(Err(Error::Field("epk")))?;
        let iv = iv.as_slice().try_into().or(Err(Error::Field("iv")))?;
        let opened = cipher.decrypt_in_place(iv, self.room.as_bytes(), &mut *text);
        opened.or(Err(Error::Sealed)).and_then(|()| receive(&text, named))
    }
}
