//! auth.md: the authentication statement, its `Authorization` header, and its checks.

mod common;

use common::{AT, DID, fields, map, seal, secret, text};
use multibase::Base;
use zu::auth::{WINDOW, sign, verify};
use zu::{Error, PublicKey, Value};

const HOST: &str = "https://pol.4seas.example";
const NONCE: [u8; 16] = [7; 16];

fn any_key(_: &str, _: &PublicKey) -> bool {
    true
}

fn unseen(_: &str, _: &[u8]) -> bool {
    true
}

fn statement(header: &str) -> Value {
    Value::decode(&Base::Base64Url.decode(header.strip_prefix("Zu ").unwrap()).unwrap()).unwrap()
}

fn header(statement: &Value) -> String {
    format!("Zu {}", Base::Base64Url.encode(statement.encode()))
}

/// auth.md § Statement: the example of the specification, with its elided values filled in,
/// and § Sending a Statement: base64url without padding, under the scheme `Zu`.
#[test]
fn the_statement_has_exactly_the_fields_of_the_specification() {
    let sent = sign(&secret(1), DID, HOST, AT, &NONCE).unwrap();
    assert!(sent.starts_with("Zu ") && !sent.contains(['=', '+', '/']), "{sent}");
    let signed = fields(statement(&sent));
    let Some(Value::Bytes(sig)) = signed.get("sig").cloned() else { panic!("no sig") };
    let expected = map([
        ("$type", text("auth")),
        ("v", Value::Int(1)),
        ("by", text("did:zu:ewvi7nxzyoun6zhxrhs64oiz")),
        ("aud", text("https://pol.4seas.example")),
        ("at", Value::Int(1794528000000)),
        ("nonce", Value::Bytes(NONCE.to_vec())),
        ("pub", text(&secret(1).public().to_string())),
        ("sig", Value::Bytes(sig.clone())),
    ]);
    assert_eq!(Value::Map(signed.clone()), expected);
    // The signature is over the statement without `sig`, in DRISL.
    let mut unsigned = signed;
    unsigned.remove("sig");
    assert_eq!(secret(1).public().verify(&Value::Map(unsigned).encode(), &sig), Ok(()));
    assert_eq!(verify(&sent, HOST, AT, any_key, unseen), Ok(DID.to_owned()));
}

#[test]
fn a_statement_is_for_one_host() {
    let sent = sign(&secret(1), DID, HOST, AT, &NONCE).unwrap();
    for aud in [
        "https://other.4seas.example",
        "http://pol.4seas.example",
        "https://pol.4seas.example:8443",
        "https://pol.4seas.example/",
        "",
    ] {
        assert_eq!(verify(&sent, aud, AT, any_key, unseen), Err(Error::Audience), "{aud}");
    }
}

/// § Verifying a Statement: `at` is within 5 minutes of the host's clock, before or after.
#[test]
fn a_statement_counts_for_five_minutes_before_and_after() {
    assert_eq!(WINDOW, 300_000);
    let sent = sign(&secret(1), DID, HOST, AT, &NONCE).unwrap();
    for now in [AT, AT + 300_000, AT - 300_000, AT + 1, AT - 299_999] {
        assert_eq!(verify(&sent, HOST, now, any_key, unseen), Ok(DID.to_owned()), "{}", now - AT);
    }
    for now in [AT + 300_001, AT - 300_001, AT + 86_400_000, 0, i64::MAX, i64::MIN] {
        assert_eq!(
            verify(&sent, HOST, now, any_key, unseen),
            Err(Error::Time),
            "{}",
            now.wrapping_sub(AT)
        );
    }
}

/// § Verifying a Statement: the nonce memory is the host's. It is asked last, with the `by`
/// and the nonce of a statement that passed everything else.
#[test]
fn the_host_is_asked_about_the_nonce_last() {
    let sent = sign(&secret(1), DID, HOST, AT, &NONCE).unwrap();
    let mut memory: Vec<(String, Vec<u8>)> = Vec::new();
    let mut remember = |by: &str, nonce: &[u8]| {
        let fresh = !memory.contains(&(by.to_owned(), nonce.to_vec()));
        memory.push((by.to_owned(), nonce.to_vec()));
        fresh
    };
    assert_eq!(verify(&sent, HOST, AT, any_key, &mut remember), Ok(DID.to_owned()));
    assert_eq!(verify(&sent, HOST, AT + 1, any_key, &mut remember), Err(Error::Replay));
    // The same nonce under another DID is another statement.
    let other = sign(&secret(1), "did:web:pol.4seas.example", HOST, AT, &NONCE).unwrap();
    assert_eq!(
        verify(&other, HOST, AT, any_key, &mut remember),
        Ok("did:web:pol.4seas.example".to_owned())
    );
    assert_eq!(memory.len(), 3);
    // Statements that fail an earlier check never reach the memory.
    let never =
        |_: &str, _: &[u8]| -> bool { panic!("asked about a statement that does not pass") };
    assert_eq!(verify(&sent, "https://other.example", AT, any_key, never), Err(Error::Audience));
    assert_eq!(verify(&sent, HOST, AT + WINDOW as i64 + 1, any_key, never), Err(Error::Time));
    assert_eq!(verify(&sent, HOST, AT, |_, _| false, never), Err(Error::Signer));
}

/// § Verifying a Statement: `pub` is a Zu signing key in the current DID document of `by`.
#[test]
fn the_key_must_be_a_current_key_of_the_did() {
    let sent = sign(&secret(1), DID, HOST, AT, &NONCE).unwrap();
    let mut asked = None;
    let current = secret(1).public();
    assert_eq!(
        verify(
            &sent,
            HOST,
            AT,
            |did, key| asked.insert((did.to_owned(), *key)).1 == current,
            unseen
        ),
        Ok(DID.to_owned())
    );
    assert_eq!(asked, Some((DID.to_owned(), current)));
    assert_eq!(
        verify(&sent, HOST, AT, |_, key| *key == secret(2).public(), unseen),
        Err(Error::Signer)
    );
}

#[test]
fn a_changed_statement_does_not_pass() {
    let sent = sign(&secret(1), DID, HOST, AT, &NONCE).unwrap();
    let never = |_: &str, _: &PublicKey| -> bool { panic!("asked about a forged statement") };
    let changes: [(&str, Value); 5] = [
        ("by", text("did:zu:aaaaaaaaaaaaaaaaaaaaaaaa")),
        ("at", Value::Int(AT + 1)),
        ("nonce", Value::Bytes(vec![8; 16])),
        ("pub", text(&secret(2).public().to_string())),
        ("more", Value::Int(1)),
    ];
    for (name, value) in changes {
        let mut changed = fields(statement(&sent));
        changed.insert(name.to_owned(), value);
        assert_eq!(
            verify(&header(&Value::Map(changed)), HOST, AT, never, unseen),
            Err(Error::Signature),
            "{name}"
        );
    }
    // A statement for another host, changed to name this one.
    let elsewhere = sign(&secret(1), DID, "https://other.4seas.example", AT, &NONCE).unwrap();
    let mut changed = fields(statement(&elsewhere));
    changed.insert("aud".to_owned(), text(HOST));
    assert_eq!(
        verify(&header(&Value::Map(changed)), HOST, AT, never, unseen),
        Err(Error::Signature)
    );
    // A record header is not a statement, and a statement is not a record (`$type`).
    let record = common::record("k", AT, &[]).header();
    assert_eq!(verify(&header(&record), HOST, AT, never, unseen), Err(Error::Field("$type")));
    assert_eq!(zu::Record::try_from(statement(&sent)), Err(Error::Field("$type")));
}

#[test]
fn refuses_statements_of_the_wrong_structure() {
    let good = || {
        fields(map([
            ("$type", text("auth")),
            ("v", Value::Int(1)),
            ("by", text(DID)),
            ("aud", text(HOST)),
            ("at", Value::Int(AT)),
            ("nonce", Value::Bytes(NONCE.to_vec())),
        ]))
    };
    let check = |statement| {
        verify(&header(&seal(&secret(1), Value::Map(statement))), HOST, AT, any_key, unseen)
    };
    assert_eq!(check(good()), Ok(DID.to_owned()));
    for name in ["$type", "v", "by", "aud", "at", "nonce"] {
        let mut statement = good();
        statement.remove(name);
        assert_eq!(check(statement), Err(Error::Field(name)), "without {name}");
    }
    let wrong = [
        ("$type", text("record")),
        ("v", Value::Int(2)),
        ("by", Value::Null),
        ("aud", Value::List(vec![text(HOST)])),
        ("at", text("now")),
        ("nonce", text("AAAAAAAAAAAAAAAAAAAAAA")),
        ("nonce", Value::Bytes(vec![7; 15])),
        ("nonce", Value::Bytes(vec![])),
    ];
    for (name, value) in wrong {
        let mut statement = good();
        statement.insert(name.to_owned(), value.clone());
        assert_eq!(check(statement), Err(Error::Field(name)), "{name} = {value:?}");
    }
    // A longer nonce is welcome, and a field this software does not know is signed with the rest.
    let mut statement = good();
    statement.insert("nonce".to_owned(), Value::Bytes(vec![7; 32]));
    statement.insert("$future".to_owned(), Value::Int(1));
    assert_eq!(check(statement), Ok(DID.to_owned()));
    // The signer makes the nonce: 16 bytes at least.
    assert_eq!(sign(&secret(1), DID, HOST, AT, &[7; 15]), Err(Error::Field("nonce")));
    // `pub` and `sig`.
    let signed = fields(statement_of(&secret(1)));
    for (name, value, error) in [
        ("pub", Value::Null, Error::Field("pub")),
        ("pub", text("z6Mk"), Error::Key),
        ("sig", Value::Bytes(vec![0; 63]), Error::Field("sig")),
        ("sig", Value::Bytes(vec![0; 64]), Error::Signature),
    ] {
        let mut statement = signed.clone();
        statement.insert(name.to_owned(), value);
        assert_eq!(
            verify(&header(&Value::Map(statement)), HOST, AT, any_key, unseen),
            Err(error),
            "{name}"
        );
    }
}

fn statement_of(secret: &zu::SecretKey) -> Value {
    statement(&sign(secret, DID, HOST, AT, &NONCE).unwrap())
}

/// § Sending a Statement: `Zu`, a space, base64url without padding of the DRISL bytes.
#[test]
fn refuses_headers_of_another_form() {
    let sent = sign(&secret(1), DID, HOST, AT, &NONCE).unwrap();
    let token = &sent[3..];
    let standard = Base::Base64.encode(statement_of(&secret(1)).encode());
    assert_ne!(standard, token, "the statement has bytes that the two alphabets write differently");
    let header_error = Err(Error::Field("Authorization"));
    let refused = [
        (String::new(), header_error.clone()),
        ("Zu".to_owned(), header_error.clone()),
        (token.to_owned(), header_error.clone()),
        (format!("Bearer {token}"), header_error.clone()),
        (format!("Zu  {token}"), header_error.clone()),
        (format!("Zu {token}="), header_error.clone()),
        (format!("Zu {token} "), header_error.clone()),
        (format!("Zu {standard}"), header_error.clone()),
        ("Zu !!!".to_owned(), header_error.clone()),
        ("Zu ".to_owned(), Err(Error::Drisl)),
        ("Zu 9g".to_owned(), Err(Error::Field("$type"))),
        (
            format!(
                "Zu {}",
                Base::Base64Url.encode([&statement_of(&secret(1)).encode()[..], &[0]].concat())
            ),
            Err(Error::Drisl),
        ),
    ];
    for (header, error) in refused {
        assert_eq!(verify(&header, HOST, AT, any_key, unseen), error, "{header:.40}");
    }
}
