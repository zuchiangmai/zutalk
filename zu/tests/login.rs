//! login.md: the messages of a login, on the same device and between devices.

mod common;

use common::{AT, DID, fields, hex, map, secret, text};
use multibase::Base::Base64Url;
use proptest::prelude::*;
use zu::login::{Code, finder, key, page, receive, request, requested, scan};
use zu::{Doc, Error, PublicKey, Value};

const ORIGIN: &str = "https://pol.4seas.example";
const SEED: [u8; 32] = [7; 32];
/// What the receiving page draws for one login: its key pair and its room number.
const DRAWN: [u8; 32] = [0x11; 32];
const ROOM: [u8; 16] = [0xa5; 16];
/// What the login page draws: its key pair and its nonce.
const SENT: [u8; 32] = [0x22; 32];
const IV: [u8; 12] = [0x33; 12];

/// The document of a DID with these Zu signing keys.
fn doc(did: &str, keys: &[(PublicKey, Option<i64>)]) -> Doc {
    Doc { did: did.into(), keys: keys.to_vec(), handles: vec![], services: Default::default() }
}

fn current() -> Doc {
    doc(DID, &[(secret(7).public(), None)])
}

fn code() -> Code {
    Code::new(ORIGIN, &DRAWN, &ROOM).unwrap()
}

fn sealed() -> Vec<u8> {
    scan(&code().url).unwrap().seal(&key(DID, &SEED), &SENT, &IV).unwrap()
}

/// The seed that a login hands out, or why it hands out none.
fn seed(received: Result<zu::login::Received, Error>, doc: &Doc) -> Result<[u8; 32], Error> {
    received?.seed(doc).copied()
}

/// One login between devices as WebCrypto computes it (`fixtures/login/webcrypto.mjs`), with
/// the DRISL bytes written out by hand: ECDH on P-256, HKDF-SHA-256, AES-256-GCM.
#[test]
fn a_login_between_devices_is_byte_for_byte_what_webcrypto_makes() {
    let path = format!("{}/tests/fixtures/login/webcrypto.json", env!("CARGO_MANIFEST_DIR"));
    let vector: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let bytes = |name: &str| hex(vector[name].as_str().unwrap());
    let array = |name: &str| <[u8; 32]>::try_from(bytes(name)).unwrap();
    let (origin, did) = (vector["origin"].as_str().unwrap(), vector["did"].as_str().unwrap());
    // § The Code.
    let room = <[u8; 16]>::try_from(bytes("room")).unwrap();
    let code = Code::new(origin, &array("receiverSecret"), &room).unwrap();
    assert_eq!(code.url, vector["code"].as_str().unwrap());
    // § Messages.
    let message = key(did, &array("seed"));
    assert_eq!(*message, bytes("keyMessage"));
    // § Sealing the Key.
    let iv = <[u8; 12]>::try_from(bytes("iv")).unwrap();
    let sealed = scan(&code.url).unwrap().seal(&message, &array("senderSecret"), &iv).unwrap();
    assert_eq!(sealed, bytes("sealed"));
    // § Receiving the Key.
    let received = code.open(&bytes("sealed"), Some(did));
    assert_eq!(seed(received, &doc(did, &[(secret(7).public(), None)])), Ok(array("seed")));
}

/// § Messages: the key message is a map with `$type`, `v`, `did` and `key`, in DRISL.
#[test]
fn the_key_message_has_exactly_the_fields_of_the_specification() {
    let expected = map([
        ("$type", text("key")),
        ("v", Value::Int(1)),
        ("did", text(DID)),
        ("key", Value::Bytes(SEED.to_vec())),
    ]);
    assert_eq!(Value::decode(&key(DID, &SEED)), Ok(expected));
    // Whatever the length of the DID, the seed lands in `key`.
    for did in
        ["did:web:a.co", "did:web:user.example.com", &format!("did:web:{}.com", "a".repeat(300))]
    {
        let seed: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
        let message = fields(Value::decode(&key(did, &seed)).unwrap());
        assert_eq!(message["did"], text(did));
        assert_eq!(message["key"], Value::Bytes(seed.to_vec()));
    }
}

/// § Messages: a request is a map with `$type` and `v`, and `did` when the receiving page
/// knows which DID the person wants to use.
#[test]
fn a_request_names_the_did_the_person_wants_or_none() {
    let named = map([("$type", text("login")), ("v", Value::Int(1)), ("did", text(DID))]);
    assert_eq!(Value::decode(&request(Some(DID))), Ok(named));
    assert_eq!(
        Value::decode(&request(None)),
        Ok(map([("$type", text("login")), ("v", Value::Int(1))]))
    );
    assert_eq!(requested(&request(Some(DID))), Ok(Some(DID.to_owned())));
    assert_eq!(requested(&request(None)), Ok(None));
}

#[test]
fn a_request_of_another_type_version_or_shape_is_refused() {
    let request = |fields: Value| requested(&fields.encode());
    let good = [("$type", text("login")), ("v", Value::Int(1))];
    assert_eq!(request(map(good.clone())), Ok(None));
    let refused = [
        (map([("$type", text("key")), ("v", Value::Int(1))]), Error::Field("$type")),
        (map([("$type", text("Login")), ("v", Value::Int(1))]), Error::Field("$type")),
        (map([("v", Value::Int(1))]), Error::Field("$type")),
        (map([("$type", text("login")), ("v", Value::Int(2))]), Error::Field("v")),
        (map([("$type", text("login")), ("v", text("1"))]), Error::Field("v")),
        (map([("$type", text("login"))]), Error::Field("v")),
        (map([good[0].clone(), good[1].clone(), ("did", Value::Null)]), Error::Field("did")),
        (
            map([good[0].clone(), good[1].clone(), ("did", text("someone.4seas.example"))]),
            Error::Field("did"),
        ),
        (
            map([
                good[0].clone(),
                good[1].clone(),
                ("did", text("did:plc:ewvi7nxzyoun6zhxrhs64oiz")),
            ]),
            Error::Field("did"),
        ),
        (Value::List(vec![]), Error::Field("$type")),
    ];
    for (fields, error) in refused {
        assert_eq!(request(fields.clone()), Err(error), "{fields:?}");
    }
    assert_eq!(requested(b"zu-login-ready"), Err(Error::Drisl));
    assert_eq!(requested(br#"{"$type":"login","v":1}"#), Err(Error::Drisl));
    // data-model.md § Data Types: a `$` field that is not known is ignored.
    assert_eq!(
        request(map([good[0].clone(), good[1].clone(), ("$later", Value::Int(1))])),
        Ok(None)
    );
}

/// § Finding the Login Page: the handle's domain is asked at `/.well-known/zu-login`, and
/// answers with the `https` URL of the login page as plain text with no wrapper.
#[test]
fn the_login_page_is_found_at_the_domain_of_the_handle() {
    let origin = zu::did::origin("someone.4seas.test", true).unwrap();
    assert_eq!(finder(&origin), "https://someone.4seas.test/.well-known/zu-login");
    // The login page may be in another origin than the handle's domain.
    for url in ["https://4seas.example/login", "https://login.example.com", "https://a.co/l?x=1#y"]
    {
        assert_eq!(page(url), Ok(url));
    }
    let refused = [
        "http://4seas.example/login",
        "HTTPS://4seas.example/login",
        "https://4seas.example/login\n",
        " https://4seas.example/login",
        "https://4seas.example/log in",
        "\"https://4seas.example/login\"",
        "{\"url\":\"https://4seas.example/login\"}",
        "<a href=\"https://4seas.example/login\">login</a>",
        "https://",
        "https:///login",
        "https://4seas.example/登录",
        "4seas.example/login",
        "",
    ];
    for body in refused {
        assert_eq!(page(body), Err(Error::Url), "{body:?}");
    }
}

/// § Receiving the Key: `$type` is `key`, `v` is `1`, and `did` is the DID the person named
/// if the person named one.
#[test]
fn a_key_message_is_checked_before_its_key_is_looked_at() {
    let message = key(DID, &SEED);
    assert_eq!(receive(&message, None).map(|received| received.did), Ok(DID.to_owned()));
    assert_eq!(receive(&message, Some(DID)).map(|received| received.did), Ok(DID.to_owned()));
    let other = "did:zu:q4cm5j2xk7hvn3oaz6t2wplb";
    assert_eq!(receive(&message, Some(other)).err(), Some(Error::Field("did")));
    let good = fields(Value::decode(&message).unwrap());
    let changes = [
        ("$type", Some(text("login")), Error::Field("$type")),
        ("$type", Some(text("sealed")), Error::Field("$type")),
        ("$type", None, Error::Field("$type")),
        ("v", Some(Value::Int(2)), Error::Field("v")),
        ("v", Some(Value::Int(0)), Error::Field("v")),
        ("v", None, Error::Field("v")),
        ("did", Some(text("did:plc:ewvi7nxzyoun6zhxrhs64oiz")), Error::Field("did")),
        ("did", Some(text("someone.4seas.example")), Error::Field("did")),
        ("did", Some(Value::Bytes(DID.into())), Error::Field("did")),
        ("did", None, Error::Field("did")),
        ("key", Some(Value::Bytes(vec![7; 31])), Error::Field("key")),
        ("key", Some(Value::Bytes(vec![7; 33])), Error::Field("key")),
        ("key", Some(Value::Bytes(vec![7; 64])), Error::Field("key")),
        ("key", Some(Value::Bytes(vec![])), Error::Field("key")),
        ("key", Some(text("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc")), Error::Field("key")),
        ("key", None, Error::Field("key")),
    ];
    for (name, to, error) in changes {
        let mut changed = good.clone();
        match to.clone() {
            Some(to) => changed.insert(name.to_owned(), to),
            None => changed.remove(name),
        };
        let got = receive(&Value::Map(changed).encode(), None).err();
        assert_eq!(got, Some(error), "{name}: {to:?}");
    }
    assert_eq!(receive(b"", None).err(), Some(Error::Drisl));
    assert_eq!(receive(&Value::Int(1).encode(), None).err(), Some(Error::Field("$type")));
}

/// § Receiving the Key: the public key of `key` is one of the Zu signing keys in the DID
/// document of `did`. A key that does not match is not handed out.
#[test]
fn the_key_is_handed_out_only_if_the_document_of_its_did_lists_it() {
    let received = || receive(&key(DID, &SEED), None);
    assert_eq!(seed(received(), &current()), Ok(SEED));
    // Among other keys.
    let several = doc(DID, &[(secret(1).public(), None), (secret(7).public(), None)]);
    assert_eq!(seed(received(), &several), Ok(SEED));
    // Another key; no key; a key that was removed; the document of another DID.
    let not_listed = [
        doc(DID, &[(secret(1).public(), None)]),
        doc(DID, &[]),
        doc(DID, &[(secret(7).public(), Some(AT))]),
        doc(DID, &[(secret(7).public(), Some(i64::MAX))]),
        doc("did:zu:q4cm5j2xk7hvn3oaz6t2wplb", &[(secret(7).public(), None)]),
    ];
    for doc in not_listed {
        assert_eq!(seed(received(), &doc), Err(Error::Signer), "{doc:?}");
    }
}

/// § The Code: `https://<receiving origin>/.well-known/zu-relay/<room>#<public key>`, the
/// room number as 32 lowercase hexadecimal characters, the public key as the 65 bytes of an
/// uncompressed point in base64url without padding.
#[test]
fn the_code_is_the_url_of_the_specification() {
    let code =
        Code::new(ORIGIN, &DRAWN, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xa, 0xb, 0xc, 0xd, 0xe, 0xff])
            .unwrap();
    assert_eq!(
        code.room,
        "https://pol.4seas.example/.well-known/zu-relay/000102030405060708090a0b0c0d0eff"
    );
    let (room, fragment) = code.url.split_once('#').unwrap();
    assert_eq!(room, code.room);
    let point = Base64Url.decode(fragment).unwrap();
    assert_eq!((point.len(), point[0], fragment.len()), (65, 4, 87));
    // The login page reads the receiving origin and the room from it.
    let scanned = scan(&code.url).unwrap();
    assert_eq!((scanned.origin.as_str(), scanned.room.as_str()), (ORIGIN, room));
    // Another draw, another key.
    assert_ne!(
        Code::new(ORIGIN, &SENT, &ROOM).unwrap().url,
        Code::new(ORIGIN, &DRAWN, &ROOM).unwrap().url
    );
    // A port is part of an origin.
    let local = Code::new("https://localhost:8443", &DRAWN, &ROOM).unwrap();
    assert_eq!(scan(&local.url).unwrap().origin, "https://localhost:8443");
}

#[test]
fn a_code_of_another_form_is_refused() {
    let good = code().url;
    let (room, fragment) = good.split_once('#').unwrap();
    assert!(scan(&good).is_ok());
    let compressed =
        Base64Url.encode([&[2u8][..], &Base64Url.decode(fragment).unwrap()[1..33]].concat());
    let off_curve = Base64Url.encode([&[4u8][..], &[1; 64]].concat());
    let refused = [
        room.to_owned(),
        format!("{room}#"),
        format!("{room}#{fragment}="),
        format!("{room}#{}", &fragment[..86]),
        format!("{room}#{}B", &fragment[..86]),
        format!("{room}#{compressed}"),
        format!("{room}#{off_curve}"),
        format!("{room}#{}", Base64Url.encode([0u8; 65])),
        format!("{room}?x=1#{fragment}"),
        format!("{room}/#{fragment}"),
        format!("{}#{fragment}", &room[..room.len() - 1]),
        format!("{room}0#{fragment}"),
        format!("{}#{fragment}", room.replace("a5a5", "A5A5")),
        format!("{}#{fragment}", room.replace("a5a5", "g5a5")),
        good.replace("https://", "http://"),
        good.replace("https://", "zu://"),
        good.replace("https://pol.4seas.example", "https://"),
        good.replace("https://pol.4seas.example", "https://pol.4seas.example/path"),
        good.replace("https://pol.4seas.example", "https://user@pol.4seas.example"),
        good.replace("zu-relay", "zu-login"),
        good.replace("/.well-known", ""),
        String::new(),
    ];
    for code in refused {
        assert_eq!(scan(&code).err(), Some(Error::Url), "{code}");
    }
    for origin in
        ["http://pol.4seas.example", "pol.4seas.example", "https://pol.4seas.example/", ""]
    {
        assert_eq!(Code::new(origin, &DRAWN, &ROOM).err(), Some(Error::Url), "{origin}");
    }
}

/// § The Code, § Sealing the Key: a draw of 32 bytes that is no P-256 private key is refused.
#[test]
fn a_draw_that_is_no_private_key_is_refused() {
    let order = hex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");
    let order = <[u8; 32]>::try_from(order).unwrap();
    let mut below = order;
    below[31] -= 1;
    assert!(Code::new(ORIGIN, &below, &ROOM).is_ok());
    for draw in [[0; 32], order, [0xff; 32]] {
        assert_eq!(Code::new(ORIGIN, &draw, &ROOM).err(), Some(Error::Key));
        assert_eq!(scan(&code().url).unwrap().seal(&key(DID, &SEED), &draw, &IV), Err(Error::Key));
    }
}

/// § Sealing the Key: the sealed message is a map with `$type`, `v`, `epk`, `iv` and `ct`,
/// the ciphertext with the 16 bytes of its tag at its end.
#[test]
fn the_sealed_message_has_exactly_the_fields_of_the_specification() {
    let message = fields(Value::decode(&sealed()).unwrap());
    let names: Vec<&str> = message.keys().map(String::as_str).collect();
    assert_eq!(names, ["$type", "ct", "epk", "iv", "v"]);
    assert_eq!((&message["$type"], &message["v"]), (&text("sealed"), &Value::Int(1)));
    assert_eq!(message["iv"], Value::Bytes(IV.to_vec()));
    let (Value::Bytes(epk), Value::Bytes(ct)) = (&message["epk"], &message["ct"]) else { panic!() };
    assert_eq!((epk.len(), epk[0]), (65, 4));
    assert_eq!(ct.len(), key(DID, &SEED).len() + 16);
    // The key is not in it in the clear, and nothing else of the key message is.
    let bytes = sealed();
    assert!(!bytes.windows(8).any(|window| window == [7; 8]));
    assert!(!bytes.windows(DID.len()).any(|window| window == DID.as_bytes()));
    assert!(sealed().len() <= 64 * 1024);
}

/// § The Relay, § Receiving the Key: the receiving page derives the same key and decrypts.
#[test]
fn the_receiver_opens_what_was_sealed_for_its_code() {
    assert_eq!(seed(code().open(&sealed(), Some(DID)), &current()), Ok(SEED));
    assert_eq!(seed(code().open(&sealed(), None), &current()), Ok(SEED));
    // The checks of § Receiving the Key apply to a key that came sealed.
    let other = "did:zu:q4cm5j2xk7hvn3oaz6t2wplb";
    assert_eq!(code().open(&sealed(), Some(other)).err(), Some(Error::Field("did")));
    assert_eq!(seed(code().open(&sealed(), None), &doc(DID, &[])), Err(Error::Signer));
    let login = scan(&code().url).unwrap().seal(&request(Some(DID)), &SENT, &IV).unwrap();
    assert_eq!(code().open(&login, None).err(), Some(Error::Field("$type")));
}

/// § Sealing the Key: the key is sealed to the public key in the code, for the room in the
/// code. Security Considerations: a host that alters the sealed message makes it fail.
#[test]
fn a_sealed_message_opens_for_no_other_code_and_not_when_altered() {
    let good = sealed();
    // Another key pair with the same room; the same key pair with another room or origin.
    let others = [
        Code::new(ORIGIN, &SENT, &ROOM),
        Code::new(ORIGIN, &DRAWN, &[0x5a; 16]),
        Code::new("https://other.4seas.example", &DRAWN, &ROOM),
    ];
    for other in others {
        assert_eq!(other.unwrap().open(&good, None).err(), Some(Error::Sealed));
    }
    // One bit of the ciphertext, of its tag, of the nonce.
    let message = fields(Value::decode(&good).unwrap());
    let flipped = |name: &str, at: usize| {
        let mut changed = message.clone();
        let Some(Value::Bytes(bytes)) = changed.get_mut(name) else { panic!() };
        let at = if at == usize::MAX { bytes.len() - 1 } else { at };
        bytes[at] ^= 1;
        Value::Map(changed).encode()
    };
    for (name, at) in [("ct", 0), ("ct", 40), ("ct", usize::MAX), ("iv", 0), ("iv", 11)] {
        assert_eq!(code().open(&flipped(name, at), None).err(), Some(Error::Sealed), "{name} {at}");
    }
    // Another public key in `epk`: the key derived is another, and so is the salt.
    let Value::Bytes(other) = fields(
        Value::decode(&scan(&code().url).unwrap().seal(&key(DID, &SEED), &DRAWN, &IV).unwrap())
            .unwrap(),
    )["epk"]
        .clone()
    else {
        panic!()
    };
    let mut changed = message.clone();
    changed.insert("epk".into(), Value::Bytes(other));
    assert_eq!(code().open(&Value::Map(changed).encode(), None).err(), Some(Error::Sealed));
}

#[test]
fn a_sealed_message_of_another_shape_is_refused() {
    let good = fields(Value::decode(&sealed()).unwrap());
    let Value::Bytes(epk) = good["epk"].clone() else { panic!() };
    let changes = [
        ("$type", Some(text("key")), Error::Field("$type")),
        ("$type", None, Error::Field("$type")),
        ("v", Some(Value::Int(2)), Error::Field("v")),
        ("v", None, Error::Field("v")),
        ("epk", Some(Value::Bytes(epk[..64].to_vec())), Error::Field("epk")),
        ("epk", Some(Value::Bytes([&[2u8][..], &epk[1..33]].concat())), Error::Field("epk")),
        ("epk", Some(Value::Bytes([&[4u8][..], &[1; 64]].concat())), Error::Field("epk")),
        ("epk", Some(Value::Bytes(vec![0; 65])), Error::Field("epk")),
        ("epk", Some(text("epk")), Error::Field("epk")),
        ("epk", None, Error::Field("epk")),
        ("iv", Some(Value::Bytes(vec![0x33; 11])), Error::Field("iv")),
        ("iv", Some(Value::Bytes(vec![0x33; 13])), Error::Field("iv")),
        ("iv", Some(Value::Bytes(vec![0x33; 16])), Error::Field("iv")),
        ("iv", None, Error::Field("iv")),
        ("ct", Some(Value::Bytes(vec![])), Error::Sealed),
        ("ct", Some(Value::Bytes(vec![0; 15])), Error::Sealed),
        ("ct", Some(Value::Bytes(vec![0; 16])), Error::Sealed),
        ("ct", Some(text("ct")), Error::Field("ct")),
        ("ct", None, Error::Field("ct")),
    ];
    for (name, to, error) in changes {
        let mut changed = good.clone();
        match to.clone() {
            Some(to) => changed.insert(name.to_owned(), to),
            None => changed.remove(name),
        };
        let got = code().open(&Value::Map(changed).encode(), None).err();
        assert_eq!(got, Some(error), "{name}: {to:?}");
    }
    assert_eq!(code().open(b"not drisl", None).err(), Some(Error::Drisl));
    assert_eq!(code().open(&key(DID, &SEED), None).err(), Some(Error::Field("$type")));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Whatever is drawn, the key that was sealed is the key that is opened, and only by
    /// the code it was sealed for.
    #[test]
    fn any_key_sealed_for_a_code_opens_for_it(
        seed: [u8; 32], drawn: [u8; 32], room: [u8; 16], sent: [u8; 32], iv: [u8; 12], other: [u8; 32],
    ) {
        prop_assume!(drawn != other);
        let (Ok(code), Ok(stranger)) = (Code::new(ORIGIN, &drawn, &room), Code::new(ORIGIN, &other, &room)) else {
            return Ok(());
        };
        let Ok(sealed) = scan(&code.url).unwrap().seal(&key(DID, &seed), &sent, &iv) else { return Ok(()) };
        let listed = doc(DID, &[(zu::SecretKey::from_seed(&seed).public(), None)]);
        prop_assert_eq!(self::seed(code.open(&sealed, Some(DID)), &listed), Ok(seed));
        prop_assert_eq!(stranger.open(&sealed, None).err(), Some(Error::Sealed));
    }

    /// Whatever arrives as a message, as a sealed message or as a code, it is an answer.
    #[test]
    fn any_bytes_are_refused_without_a_panic(bytes in proptest::collection::vec(any::<u8>(), 0..300), text in ".{0,120}") {
        prop_assert!(receive(&bytes, None).is_err());
        prop_assert!(code().open(&bytes, None).is_err());
        let _ = (requested(&bytes), scan(&text), page(&text), Code::new(&text, &DRAWN, &ROOM).is_ok());
    }
}
