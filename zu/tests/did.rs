//! did.md, did-zu.md: DID syntax, handles, documents, and the `did:zu` operation log.

mod common;
mod oplog;

use common::{AT, DID, secret};
use oplog::{Rotation, cid, did, log, operation, rotation, sign, stamp, tombstone, value, with};
use proptest::prelude::*;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use zu::did::{DIRECTORY, Doc, RECOVERY, handle, is_did, origin, resolve, url};
use zu::{Error, PublicKey};

const WEB: &str = "did:web:user.example.com";
const HOUR: i64 = 60 * 60 * 1000;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/did-zu/{name}.json", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).unwrap()
}

/// The DID that the entries of a fixture state.
fn stated(log: &[u8]) -> String {
    let json: Json = serde_json::from_slice(log).unwrap();
    json[0]["did"].as_str().unwrap().to_owned()
}

fn key(n: u8) -> PublicKey {
    secret(n).public()
}

/// A log of one operation: a DID with these Zu signing keys and these `alsoKnownAs` entries.
fn genesis(signing: &[(&str, PublicKey)], aka: &[&str]) -> Json {
    sign(operation(&[&rotation(1), &rotation(2)], signing, aka, None), &rotation(1))
}

/// A log whose operations follow one another an hour apart, each signed by `rotation(1)`.
fn chain(steps: &[&[(&str, PublicKey)]]) -> (String, Vec<Json>) {
    let mut ops: Vec<Json> = Vec::new();
    for signing in steps {
        let op = operation(&[&rotation(1), &rotation(2)], signing, &[], ops.last());
        ops.push(sign(op, &rotation(1)));
    }
    (did(&ops[0]), ops)
}

fn hourly(did: &str, ops: &[Json]) -> Vec<u8> {
    let entries: Vec<_> =
        ops.iter().enumerate().map(|(i, op)| (op, AT + i as i64 * HOUR, false)).collect();
    log(did, &entries)
}

/// did.md § DID Identifier Syntax and § Examples. `tests/path.rs` holds the full lists.
#[test]
fn a_did_is_of_valid_syntax_and_of_an_accepted_method() {
    assert!(is_did(DID) && is_did(WEB) && is_did("did:web:localhost%3A8080"));
    for not in ["did:plc:ewvi7nxzyoun6zhxrhs64oiz", "did:zu:", "did:ZU:val", "did:zu:val#zu", ""] {
        assert!(!is_did(not), "{not}");
    }
}

/// did-zu.md § Audit Logs, § Relationship to `did:plc`: the audit log, at the one directory.
#[test]
fn a_did_zu_is_resolved_against_its_own_directory() {
    assert_eq!(DIRECTORY, "https://plc.zuzalu.ink");
    let expected = "https://plc.zuzalu.ink/did:zu:ewvi7nxzyoun6zhxrhs64oiz/log/audit";
    assert_eq!(url(DID, false).as_deref(), Ok(expected));
    assert_eq!(url(DID, true).as_deref(), Ok(expected));
}

/// did-zu.md § Identifier Syntax: 24 characters of base32, in lowercase.
#[test]
fn a_did_zu_identifier_is_24_characters_of_base32() {
    let refused = [
        "did:zu:ewvi7nxzyoun6zhxrhs64oi",
        "did:zu:ewvi7nxzyoun6zhxrhs64oizz",
        "did:zu:EWVI7NXZYOUN6ZHXRHS64OIZ",
        "did:zu:ewvi7nxzyoun6zhxrhs64oi1",
        "did:zu:ewvi7nxzyoun6zhxrhs64oi8",
        "did:zu:ewvi7nxzyoun6zhxrhs64o:z",
        "did:zu:val",
    ];
    for did in refused {
        assert!(is_did(did), "{did}");
        assert_eq!(url(did, false), Err(Error::Path), "{did}");
    }
}

/// did.md § Usage and Implementation Guidelines: invalid syntax and a method that is not
/// accepted are different cases. § Accepted DID Methods: `did:plc` and `did:webvh` are not.
#[test]
fn a_did_of_another_method_is_told_apart_from_bad_syntax() {
    for did in ["did:plc:ewvi7nxzyoun6zhxrhs64oiz", "did:webvh:example.com", "did:key:z6Mk"] {
        assert_eq!(url(did, false), Err(Error::Method), "{did}");
        assert_eq!(resolve(did, b"{}"), Err(Error::Method), "{did}");
    }
    for did in ["did:METHOD:val", "DID:zu:val", "did:zu:", "did:zu:val/two", "zu:val", ""] {
        assert_eq!(url(did, false), Err(Error::Path), "{did}");
        assert_eq!(resolve(did, b"{}"), Err(Error::Path), "{did}");
    }
}

/// did.md § `did:web` in Zu: the document is fetched from the domain over HTTPS.
#[test]
fn a_did_web_is_resolved_at_its_domain() {
    assert_eq!(url(WEB, false).as_deref(), Ok("https://user.example.com/.well-known/did.json"));
    let upper = url("did:web:User.Example.COM", false);
    assert_eq!(upper.as_deref(), Ok("https://user.example.com/.well-known/did.json"));
}

/// did.md § `did:web` in Zu: only a hostname, with the restrictions on top-level domains that
/// handles have; `localhost`, a port and `.test` only in testing.
#[test]
fn a_did_web_with_a_path_a_port_or_a_reserved_domain_is_not_resolved() {
    let refused = [
        "did:web:example.com:user:someone",
        "did:web:example.com%3A8080",
        "did:web:example.com%3A",
        "did:web:com",
        "did:web:pol.4seas.example",
        "did:web:laptop.local",
        "did:web:blah.arpa",
        "did:web:hidden.onion",
        "did:web:192.168.0.1",
        "did:web:-a.example.com",
    ];
    for did in refused {
        assert_eq!(url(did, false), Err(Error::Domain), "{did}");
        assert_eq!(url(did, true), Err(Error::Domain), "{did}");
    }
    let testing = [
        ("did:web:localhost", "https://localhost/.well-known/did.json"),
        ("did:web:localhost%3A8080", "https://localhost:8080/.well-known/did.json"),
        ("did:web:pol.4seas.test", "https://pol.4seas.test/.well-known/did.json"),
    ];
    for (did, expected) in testing {
        assert_eq!(url(did, true).as_deref(), Ok(expected), "{did}");
        assert_eq!(url(did, false), Err(Error::Domain), "{did}");
    }
    let never = [
        "did:web:localhost:8080",
        "did:web:localhost%3A",
        "did:web:localhost%3A99999",
        "did:web:localhost%3A80%3A80",
        "did:web:localhost8080",
    ];
    for did in never {
        assert_eq!(url(did, true), Err(Error::Domain), "{did}");
    }
}

/// map.md § Handle Identifier Syntax and § Identifier Examples.
#[test]
fn handles() {
    let valid = [
        "someone.4seas.example",
        "8.cn",
        "name.t--t",
        "XX.LCS.MIT.EDU",
        "a.co",
        "xn--notarealidn.com",
        "xn--fiqa61au8b7zsevnm8ak20mc4a87e.xn--fiqs8s",
        "xn--ls8h.test",
        "example.t",
        "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion",
        "laptop.local",
        "blah.arpa",
    ];
    for name in valid {
        assert_eq!(handle(name), Some(name.to_lowercase()), "{name}");
    }
    let long = format!("{}.com", "a".repeat(63));
    assert!(handle(&long).is_some());
    let longest =
        [["a".repeat(63), "b".repeat(63), "c".repeat(63)].join("."), "d".repeat(61)].join(".");
    assert_eq!((longest.len(), handle(&longest).is_some()), (253, true));
    let invalid = [
        "jo@hn.test",
        "💩.test",
        "john..test",
        "xn--bcher-.tld",
        "john.0",
        "cn.8",
        "www.masełkowski.pl.com",
        "org",
        "name.org.",
        ".name.org",
        "-name.org",
        "na me.org",
        "name.org\n",
        "@someone.4seas.example",
        "someone.4seas.example/",
        "someone.4seas.example:443",
        "",
        &format!("{}.com", "a".repeat(64)),
        &format!("{longest}e"),
    ];
    for name in invalid {
        assert_eq!(handle(name), None, "{name}");
    }
}

/// path.md § `zu://` URI Structure: data is looked for at the domains of the handles. map.md
/// § Additional Non-Syntax Restrictions: reserved domains fail, `.test` is for development.
#[test]
fn the_host_of_a_handle_is_at_its_domain_unless_the_domain_is_reserved() {
    assert_eq!(origin("someone.4seas.org", false).as_deref(), Ok("https://someone.4seas.org"));
    assert_eq!(origin("Someone.4seas.ORG", false).as_deref(), Ok("https://someone.4seas.org"));
    assert_eq!(origin("someone.4seas.test", true).as_deref(), Ok("https://someone.4seas.test"));
    assert_eq!(origin("someone.4seas.test", false), Err(Error::Domain));
    for top in ["alt", "arpa", "example", "internal", "invalid", "local", "localhost", "onion"] {
        let name = format!("someone.{top}");
        assert_eq!(origin(&name, false), Err(Error::Domain), "{name}");
        assert_eq!(origin(&name, true), Err(Error::Domain), "{name}");
    }
    for not in ["org", "someone.4seas.org/", "https://someone.4seas.org", "localhost", ""] {
        assert_eq!(origin(not, true), Err(Error::Domain), "{not}");
    }
}

fn method(id: &str, controller: &str, kind: &str, key: &str) -> Json {
    json!({ "id": id, "type": kind, "controller": controller, "publicKeyMultibase": key })
}

fn web(methods: Vec<Json>, aka: Json, services: Json) -> Vec<u8> {
    let doc = json!({
        "@context": ["https://www.w3.org/ns/did/v1"],
        "id": WEB,
        "alsoKnownAs": aka,
        "verificationMethod": methods,
        "service": services,
    });
    serde_json::to_vec(&doc).unwrap()
}

/// did.md § DID Documents, § Representation of Public Keys: the example, for a `did:web`.
#[test]
fn a_document_lists_zu_signing_keys_handles_and_services() {
    let methods = vec![
        method(&format!("{WEB}#zu"), WEB, "Multikey", &key(1).to_string()),
        method("#zu-7xq2k4mf", WEB, "Multikey", &key(2).to_string()),
    ];
    let aka =
        json!(["zu://someone.4seas.example", "at://someone.test", "zu://Someone.Zuzalu.example"]);
    let services = json!([
        { "id": "#relay", "type": "IrohRelay", "serviceEndpoint": "https://relay.example.com" },
        { "id": format!("{WEB}#other"), "type": "Other", "serviceEndpoint": "https://example.com" },
    ]);
    let doc = resolve(WEB, &web(methods, aka, services)).unwrap();
    let service = |id: &str, kind: &str, endpoint: &str| {
        (id.to_owned(), (kind.to_owned(), endpoint.to_owned()))
    };
    let expected = Doc {
        did: WEB.into(),
        keys: vec![(key(1), None), (key(2), None)],
        handles: vec!["someone.4seas.example".into(), "someone.zuzalu.example".into()],
        services: BTreeMap::from([
            service("relay", "IrohRelay", "https://relay.example.com"),
            service("other", "Other", "https://example.com"),
        ]),
    };
    assert_eq!(doc, expected);
    // A `did:web` keeps no history: a key counts at every time, or at none.
    assert!(doc.key_now(&key(1)) && doc.key_at(&key(1), 0) && doc.key_at(&key(2), i64::MAX));
    assert!(!doc.key_now(&key(3)) && !doc.key_at(&key(3), AT));
}

/// did.md § DID Documents: the fragment is `zu`, or `zu-` and eight characters of base32,
/// compared whole; the controller is the DID; the type is `Multikey`; the key is Ed25519.
#[test]
fn a_key_of_another_name_controller_type_or_kind_is_no_zu_signing_key() {
    let good = key(1).to_string();
    let p256 = "zDnaeh9v2RmcMo13Du2d6pjUf5bZwtauYxj3n9dYjw4EZUAR7";
    let not = [
        method("#a#zu", WEB, "Multikey", &good),
        method("#atproto", WEB, "Multikey", &good),
        method("#zu-", WEB, "Multikey", &good),
        method("#zu-7xq2k4m", WEB, "Multikey", &good),
        method("#zu-7xq2k4mfa", WEB, "Multikey", &good),
        method("#zu-7XQ2K4MF", WEB, "Multikey", &good),
        method("#zu-7xq2k4m1", WEB, "Multikey", &good),
        method("#ZU", WEB, "Multikey", &good),
        method("#zu2", WEB, "Multikey", &good),
        method("zu", WEB, "Multikey", &good),
        method("##zu", WEB, "Multikey", &good),
        method(&format!("{WEB}munity#zu"), WEB, "Multikey", &good),
        method("did:web:other.example.com#zu", WEB, "Multikey", &good),
        method("#zu", "did:web:other.example.com", "Multikey", &good),
        method("#zu", WEB, "Ed25519VerificationKey2020", &good),
        method("#zu", WEB, "Multikey", p256),
        method("#zu", WEB, "Multikey", &format!("did:key:{good}")),
        method("#zu", WEB, "Multikey", "z6Mk"),
        json!({ "id": "#zu", "type": "Multikey", "controller": WEB }),
        json!({ "id": 1, "type": "Multikey", "controller": WEB, "publicKeyMultibase": good }),
        json!("#zu"),
    ];
    for entry in not {
        let doc = resolve(WEB, &web(vec![entry.clone()], json!([]), json!([]))).unwrap();
        assert_eq!(doc.keys, vec![], "{entry}");
    }
    // The same key under two names is one key.
    let twice =
        vec![method("#zu", WEB, "Multikey", &good), method("#zu-7xq2k4mf", WEB, "Multikey", &good)];
    assert_eq!(resolve(WEB, &web(twice, json!([]), json!([]))).unwrap().keys, vec![(key(1), None)]);
}

/// did.md § DID Documents: a handle is `zu://` and a hostname, with nothing after it.
#[test]
fn only_zu_uris_with_a_valid_handle_are_claimed_handles() {
    let aka = json!([
        "at://someone.4seas.example",
        "https://someone.4seas.example",
        "zu://someone.4seas.example/",
        "zu://someone.4seas.example/ev/event",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz",
        "zu://org",
        "zu://",
        "ZU://someone.4seas.example",
        "someone.4seas.example",
        7,
        "zu://second.4seas.example",
    ]);
    let doc = resolve(WEB, &web(vec![], aka, json!([]))).unwrap();
    assert_eq!(doc.handles, ["second.4seas.example"]);
    // Parts that are missing, or of another shape, state nothing.
    let bare = serde_json::to_vec(&json!({ "id": WEB })).unwrap();
    let empty = Doc { did: WEB.into(), keys: vec![], handles: vec![], services: BTreeMap::new() };
    assert_eq!(resolve(WEB, &bare), Ok(empty.clone()));
    let odd =
        json!({ "id": WEB, "alsoKnownAs": "zu://a.co", "verificationMethod": {}, "service": 1 });
    assert_eq!(resolve(WEB, &serde_json::to_vec(&odd).unwrap()), Ok(empty));
}

/// did.md § DID Documents: the `id` of the document is checked against the DID asked for.
#[test]
fn a_document_of_another_did_is_refused() {
    let doc = web(vec![], json!([]), json!([]));
    assert_eq!(resolve("did:web:other.example.com", &doc), Err(Error::Field("id")));
    assert_eq!(resolve(WEB, b"{}"), Err(Error::Field("id")));
    assert_eq!(resolve(WEB, b"[]"), Err(Error::Field("id")));
    assert_eq!(resolve(WEB, b"not json"), Err(Error::Json));
    assert_eq!(resolve(WEB, b""), Err(Error::Json));
    // The audit log of a `did:zu` is a list.
    assert_eq!(resolve(DID, b"{}"), Err(Error::Json));
    assert_eq!(resolve(DID, b"not json"), Err(Error::Json));
    assert_eq!(resolve(DID, b"[]"), Err(Error::Log(1)));
}

/// The audit logs of `did-method-plc/go-didplc` (`testdata/`), as `did-method-zu` carries
/// them under `interop_tests/audit_log/valid`, that a validator must accept.
#[test]
fn the_interop_logs_that_are_valid_pass() {
    let valid = [
        "valid/log_bskyapp",
        "valid/log_duplicate_rotation_keys",
        "valid/log_empty_rotation_keys",
        "valid/log_nullification",
        "valid/log_nullification_at_exactly_72h",
        "valid/log_nullification_nontrivial",
        "valid/log_nullified_tombstone",
        "valid/log_tombstone",
    ];
    for name in valid {
        let log = fixture(name);
        let doc = resolve(&stated(&log), &log).unwrap_or_else(|error| panic!("{name}: {error}"));
        // These DIDs were made for atproto: they have no Zu signing key and claim no handle.
        assert_eq!((doc.keys, doc.handles), (vec![], vec![]), "{name}");
    }
    let doc =
        resolve(&stated(&fixture("valid/log_bskyapp")), &fixture("valid/log_bskyapp")).unwrap();
    let host = ("AtprotoPersonalDataServer".to_owned(), "https://bsky.social".to_owned());
    assert_eq!(doc.services, BTreeMap::from([("atproto_pds".to_owned(), host)]));
    // A DID that ends with a tombstone has nothing left.
    let gone = fixture("valid/log_tombstone");
    assert_eq!(resolve(&stated(&gone), &gone).unwrap().services, BTreeMap::new());
}

/// The same collection, `interop_tests/audit_log/invalid`: each log fails, and at the
/// operation that breaks the rule its name states.
#[test]
fn the_interop_logs_that_are_invalid_fail_at_the_operation_that_breaks_the_rule() {
    let invalid = [
        ("invalid/log_invalid_nullification_reused_key", 3),
        ("invalid/log_invalid_nullification_too_slow", 3),
        ("invalid/log_invalid_sig_b64_newline", 1),
        ("invalid/log_invalid_sig_b64_padding_bits", 1),
        ("invalid/log_invalid_sig_b64_padding_chars", 1),
        ("invalid/log_invalid_sig_k256_high_s", 1),
        ("invalid/log_invalid_sig_p256_high_s", 1),
        ("invalid/log_invalid_update_nullified", 5),
        ("invalid/log_invalid_update_tombstoned", 3),
    ];
    for (name, operation) in invalid {
        let log = fixture(name);
        assert_eq!(resolve(&stated(&log), &log), Err(Error::Log(operation)), "{name}");
    }
}

/// did-zu.md § Namespace Separation: the same log is no log of the `did:plc` of that name,
/// and a log is the log of one DID.
#[test]
fn a_log_is_checked_against_the_did_asked_for() {
    let log = fixture("valid/log_tombstone");
    let did = stated(&log);
    assert_eq!(resolve(&did.replace("did:zu:", "did:plc:"), &log), Err(Error::Method));
    assert_eq!(resolve(DID, &log), Err(Error::Log(1)));
    // An entry that states another DID, or another CID than that of its operation.
    let mut json: Json = serde_json::from_slice(&log).unwrap();
    json[1]["did"] = json!(DID);
    assert_eq!(resolve(&did, &serde_json::to_vec(&json).unwrap()), Err(Error::Log(2)));
    let mut json: Json = serde_json::from_slice(&log).unwrap();
    json[1]["cid"] = json[0]["cid"].clone();
    assert_eq!(resolve(&did, &serde_json::to_vec(&json).unwrap()), Err(Error::Log(2)));
}

/// did-zu.md § DID Creation: the DID is the start of the hash of the signed genesis
/// operation, which one of its own rotation keys signed.
#[test]
fn the_did_is_the_hash_of_the_genesis_operation() {
    let op = genesis(&[("zu", key(1))], &["zu://someone.4seas.example"]);
    let name = did(&op);
    let doc = resolve(&name, &log(&name, &[(&op, AT, false)])).unwrap();
    assert_eq!(doc.did, name);
    assert_eq!(
        (doc.keys, doc.handles),
        (vec![(key(1), None)], vec!["someone.4seas.example".to_owned()])
    );
    // The hash is over the signed operation: another signature, another DID.
    let other = sign(
        operation(
            &[&rotation(1), &rotation(2)],
            &[("zu", key(1))],
            &["zu://someone.4seas.example"],
            None,
        ),
        &rotation(2),
    );
    assert_ne!(did(&other), name);
    assert_eq!(resolve(&name, &log(&name, &[(&other, AT, false)])), Err(Error::Log(1)));
    assert!(resolve(&did(&other), &log(&did(&other), &[(&other, AT, false)])).is_ok());
    // A key that is not among its rotation keys.
    let stranger = sign(operation(&[&rotation(1)], &[], &[], None), &rotation(3));
    assert_eq!(
        resolve(&did(&stranger), &log(&did(&stranger), &[(&stranger, AT, false)])),
        Err(Error::Log(1))
    );
    // A genesis operation is the first, and the first is a genesis operation.
    let later = sign(operation(&[&rotation(1)], &[], &[], None), &rotation(1));
    assert_eq!(
        resolve(&name, &log(&name, &[(&op, AT, false), (&later, AT + 1, false)])),
        Err(Error::Log(2))
    );
    let update = sign(operation(&[&rotation(1)], &[], &[], Some(&op)), &rotation(1));
    assert_eq!(
        resolve(&did(&update), &log(&did(&update), &[(&update, AT, false)])),
        Err(Error::Log(1))
    );
    // The genesis operation itself, served again, would bring back what was changed since.
    let entries = [(&op, AT, false), (&update, AT + 1, false), (&op, AT + 2, false)];
    assert_eq!(resolve(&name, &log(&name, &entries)), Err(Error::Log(3)));
    assert_eq!(
        resolve(&name, &log(&name, &[entries[0], (&op, AT + 1, false)])),
        Err(Error::Log(2))
    );
}

/// did-zu.md § Audit Logs: an operation is signed by a rotation key of the operation it
/// follows. Keys of secp256k1 and of P-256 both sign.
#[test]
fn an_operation_is_signed_by_a_rotation_key_of_the_one_before() {
    let first = genesis(&[], &[]);
    let name = did(&first);
    for signer in [1, 2] {
        let next = sign(operation(&[&rotation(3)], &[], &[], Some(&first)), &rotation(signer));
        assert!(
            resolve(&name, &log(&name, &[(&first, AT, false), (&next, AT + 1, false)])).is_ok()
        );
        // Then the keys of that operation are the ones that count.
        let third =
            |signer: u8| sign(operation(&[&rotation(3)], &[], &[], Some(&next)), &rotation(signer));
        let entries = |third: &Json| {
            log(&name, &[(&first, AT, false), (&next, AT + 1, false), (third, AT + 2, false)])
        };
        assert!(resolve(&name, &entries(&third(3))).is_ok());
        assert_eq!(resolve(&name, &entries(&third(signer))), Err(Error::Log(3)));
    }
    // Its own new key does not sign for it, and neither does a key from nowhere.
    for signer in [3, 4, 5] {
        let next = sign(
            operation(&[&rotation(3), &rotation(4)], &[], &[], Some(&first)),
            &rotation(signer),
        );
        let entries = log(&name, &[(&first, AT, false), (&next, AT + 1, false)]);
        assert_eq!(resolve(&name, &entries), Err(Error::Log(2)), "{signer}");
    }
    // A signature over other bytes: the operation was changed after it was signed.
    let next = sign(operation(&[&rotation(3)], &[], &[], Some(&first)), &rotation(1));
    let changed = with(next, "alsoKnownAs", json!(["zu://another.4seas.example"]));
    assert_eq!(
        resolve(&name, &log(&name, &[(&first, AT, false), (&changed, AT + 1, false)])),
        Err(Error::Log(2))
    );
}

/// A genesis operation with this signature in place of its own, as a log of its own DID.
fn resigned(op: &Json, sig: &str) -> Result<Doc, Error> {
    let op = with(op.clone(), "sig", json!(sig));
    resolve(&did(&op), &log(&did(&op), &[(&op, AT, false)]))
}

/// did-zu.md § Operation Serialization: `r` and `s` as 64 bytes, `s` in the lower half, in
/// base64url without padding and with its trailing bits zero. Any other form of the same
/// signature would change the CID of the operation without invalidating the signature.
#[test]
fn a_signature_has_one_form() {
    use multibase::Base::Base64Url;
    for signer in [1, 2] {
        let unsigned = operation(&[&rotation(signer)], &[], &[], None);
        let bytes = value(&unsigned).encode();
        let low = rotation(signer).signature(&bytes, false);
        let high = rotation(signer).signature(&bytes, true);
        assert_eq!(low[..32], high[..32]);
        assert!(low[32..] < high[32..]);
        assert!(resigned(&unsigned, &Base64Url.encode(&low)).is_ok(), "{signer}");
        assert_eq!(resigned(&unsigned, &Base64Url.encode(&high)), Err(Error::Log(1)), "{signer}");
        // 64 bytes are 86 characters, the last of which carries 2 bits: `A`, `Q`, `g` or `w`.
        let text = Base64Url.encode(&low);
        let last = text.chars().last().unwrap();
        assert!("AQgw".contains(last));
        let loose = char::from(last as u8 + 1);
        let other_forms = [
            format!("{text}=="),
            format!("{text}\n"),
            format!(" {text}"),
            format!("{}{loose}", &text[..85]),
            text.replace('-', "+").replace('_', "/")
                + if text.contains(['-', '_']) { "" } else { "=" },
            multibase::Base::Base64.encode(&low) + "=",
            Base64Url.encode(&low[..63]),
            Base64Url.encode([&low[..], &[0]].concat()),
            // In the manner of DER: a sequence of two integers.
            Base64Url.encode(
                [&[0x30, 0x44, 0x02, 0x20], &low[..32], &[0x02, 0x20], &low[32..]].concat(),
            ),
            String::new(),
        ];
        for form in other_forms {
            assert_eq!(resigned(&unsigned, &form), Err(Error::Log(1)), "{signer} {form:?}");
        }
        // `r` or `s` out of range.
        for (from, fill) in [(0, 0), (32, 0), (0, 0xff), (32, 0xff)] {
            let mut odd = low.clone();
            odd[from..from + 32].fill(fill);
            assert_eq!(resigned(&unsigned, &Base64Url.encode(&odd)), Err(Error::Log(1)));
        }
    }
}

/// did-zu.md: rotation keys are `did:key`s of secp256k1 or P-256, and no other key signs.
#[test]
fn a_rotation_key_of_another_kind_signs_nothing() {
    let good = rotation(1);
    let unsigned = |keys: Json| with(operation(&[], &[], &[], None), "rotationKeys", keys);
    let made = |op: Json| resolve(&did(&op), &log(&did(&op), &[(&op, AT, false)]));
    let odd_keys = [
        json!([format!("did:key:{}", key(1))]),
        json!([good.did_key.trim_start_matches("did:key:")]),
        json!([good.did_key.replace("did:key:z", "did:key:")]),
        json!(["did:key:z"]),
        json!([]),
    ];
    for keys in odd_keys {
        assert_eq!(made(sign(unsigned(keys.clone()), &good)), Err(Error::Log(1)), "{keys}");
    }
    // A key that signs nothing does not keep a key after it from signing.
    let mixed = json!([format!("did:key:{}", key(1)), "did:key:zQ3sh", good.did_key]);
    assert!(made(sign(unsigned(mixed), &good)).is_ok());
}

/// did-zu.md § Audit Logs: `prev` is the CID of an operation earlier in the log.
#[test]
fn an_operation_follows_one_that_is_in_the_log_before_it() {
    let first = genesis(&[], &[]);
    let name = did(&first);
    let second = sign(operation(&[&rotation(1)], &[], &[], Some(&first)), &rotation(1));
    let third = sign(operation(&[&rotation(1)], &[], &[], Some(&second)), &rotation(1));
    let all = [(&first, AT, false), (&second, AT + 1, false), (&third, AT + 2, false)];
    assert!(resolve(&name, &log(&name, &all)).is_ok());
    // Without the operation it follows; before it; and after itself.
    assert_eq!(resolve(&name, &log(&name, &[all[0], (&third, AT + 2, false)])), Err(Error::Log(2)));
    assert_eq!(
        resolve(&name, &log(&name, &[all[0], (&third, AT + 1, false), (&second, AT + 2, false)])),
        Err(Error::Log(2))
    );
    assert_eq!(
        resolve(&name, &log(&name, &[all[0], all[1], (&second, AT + 2, false)])),
        Err(Error::Log(3))
    );
    // `prev` is the one string of that CID.
    for prev in
        [json!(cid(&first).to_uppercase()), json!(format!("{}a", cid(&first))), json!(7), json!([])]
    {
        let odd = sign(
            with(operation(&[&rotation(1)], &[], &[], None), "prev", prev.clone()),
            &rotation(1),
        );
        assert_eq!(
            resolve(&name, &log(&name, &[all[0], (&odd, AT + 1, false)])),
            Err(Error::Log(2)),
            "{prev}"
        );
    }
}

/// The times at which the directory recorded the operations go strictly forward.
#[test]
fn the_times_of_a_log_increase() {
    let (name, ops) = chain(&[&[], &[]]);
    assert!(resolve(&name, &log(&name, &[(&ops[0], AT, false), (&ops[1], AT + 1, false)])).is_ok());
    for at in [AT, AT - 1, 0] {
        let entries = log(&name, &[(&ops[0], AT, false), (&ops[1], at, false)]);
        assert_eq!(resolve(&name, &entries), Err(Error::Log(2)), "{at}");
    }
}

/// A DID whose genesis lists a recovery key, `rotation(2)`, above the everyday `rotation(1)`.
/// `rotation(1)` changed the handle; then the recovery key takes the DID back.
fn recovered(by: u8, after: i64, marked: [bool; 3]) -> Result<Doc, Error> {
    let first =
        sign(operation(&[&rotation(2), &rotation(1)], &[("zu", key(1))], &[], None), &rotation(1));
    let taken =
        sign(operation(&[&rotation(3)], &[("zu", key(9))], &[], Some(&first)), &rotation(1));
    let back =
        sign(operation(&[&rotation(2)], &[("zu", key(2))], &[], Some(&first)), &rotation(by));
    let name = did(&first);
    let entries = [
        (&first, AT, marked[0]),
        (&taken, AT + HOUR, marked[1]),
        (&back, AT + HOUR + after, marked[2]),
    ];
    resolve(&name, &log(&name, &entries))
}

/// did-zu.md § Key Rotation & Account Recovery: within 72 hours a rotation key of higher
/// authority can undo an operation signed by one of lower authority.
#[test]
fn a_key_of_higher_authority_undoes_an_operation_within_72_hours() {
    assert_eq!(RECOVERY, 72 * HOUR);
    let undone = [false, true, false];
    for after in [1, HOUR, RECOVERY] {
        let doc = recovered(2, after, undone).unwrap();
        // The key that the undone operation stated never was a key of the DID. The first key
        // was in the document until the recovery left it out.
        assert_eq!(doc.keys, vec![(key(1), Some(AT + HOUR + after)), (key(2), None)], "{after}");
    }
    assert_eq!(recovered(2, RECOVERY + 1, undone), Err(Error::Log(3)));
    assert_eq!(recovered(2, 100 * RECOVERY, undone), Err(Error::Log(3)));
    // Not by the key that signed it, which has the same authority, nor by one that has none.
    assert_eq!(recovered(1, HOUR, undone), Err(Error::Log(3)));
    assert_eq!(recovered(3, HOUR, undone), Err(Error::Log(3)));
}

/// did-zu.md § Audit Logs: the log marks as nullified the operations that were undone, and
/// only those.
#[test]
fn the_log_marks_exactly_the_operations_that_were_undone() {
    assert_eq!(recovered(2, HOUR, [false, false, false]), Err(Error::Log(2)));
    assert_eq!(recovered(2, HOUR, [true, true, false]), Err(Error::Log(1)));
    assert_eq!(recovered(2, HOUR, [false, true, true]), Err(Error::Log(3)));
    let (name, ops) = chain(&[&[], &[]]);
    let marked = log(&name, &[(&ops[0], AT, false), (&ops[1], AT + 1, true)]);
    assert_eq!(resolve(&name, &marked), Err(Error::Log(2)));
    // The mark is a boolean, and it is there.
    for mark in [json!(null), json!(0), json!("false")] {
        let mut json: Json = serde_json::from_slice(&hourly(&name, &ops)).unwrap();
        json[0]["nullified"] = mark;
        assert_eq!(resolve(&name, &serde_json::to_vec(&json).unwrap()), Err(Error::Log(1)));
        json[0].as_object_mut().unwrap().remove("nullified");
        assert_eq!(resolve(&name, &serde_json::to_vec(&json).unwrap()), Err(Error::Log(1)));
    }
}

/// did-zu.md § Key Rotation & Account Recovery: an operation that was undone is followed
/// by nothing, and what a recovery leaves standing cannot be undone by a lesser key.
#[test]
fn nothing_follows_an_operation_that_was_undone() {
    let first = sign(operation(&[&rotation(2), &rotation(1)], &[], &[], None), &rotation(1));
    let taken = sign(operation(&[&rotation(1)], &[], &[], Some(&first)), &rotation(1));
    let back = sign(operation(&[&rotation(2), &rotation(1)], &[], &[], Some(&first)), &rotation(2));
    let name = did(&first);
    let so_far = [(&first, AT, false), (&taken, AT + 1, true), (&back, AT + 2, false)];
    assert!(resolve(&name, &log(&name, &so_far)).is_ok());
    let onto_undone = sign(operation(&[&rotation(1)], &[], &[], Some(&taken)), &rotation(1));
    let entries = [so_far[0], so_far[1], so_far[2], (&onto_undone, AT + 3, false)];
    assert_eq!(resolve(&name, &log(&name, &entries)), Err(Error::Log(4)));
    // The recovery was signed by the highest key: no key is left that could undo it.
    for signer in [1, 2] {
        let again = sign(operation(&[&rotation(1)], &[], &[], Some(&first)), &rotation(signer));
        let entries = [so_far[0], so_far[1], (&back, AT + 2, true), (&again, AT + 3, false)];
        assert_eq!(resolve(&name, &log(&name, &entries)), Err(Error::Log(4)), "{signer}");
    }
}

/// did-zu.md § How it works: a tombstone deactivates the DID for good, unless it is undone
/// within the window like any other operation.
#[test]
fn a_tombstone_ends_a_did() {
    let first = sign(
        operation(
            &[&rotation(2), &rotation(1)],
            &[("zu", key(1))],
            &["zu://a.4seas.example"],
            None,
        ),
        &rotation(1),
    );
    let end = sign(tombstone(&first), &rotation(1));
    let name = did(&first);
    let ended = [(&first, AT, false), (&end, AT + HOUR, false)];
    let doc = resolve(&name, &log(&name, &ended)).unwrap();
    // No key is current, no handle is claimed; what was signed before stays signed.
    assert_eq!((doc.keys.clone(), doc.handles.len()), (vec![(key(1), Some(AT + HOUR))], 0));
    assert!(
        !doc.key_now(&key(1))
            && doc.key_at(&key(1), AT + HOUR)
            && !doc.key_at(&key(1), AT + HOUR + 1)
    );
    // Nothing follows a tombstone, whoever signs.
    for signer in [1, 2] {
        let after = sign(operation(&[&rotation(1)], &[], &[], Some(&end)), &rotation(signer));
        let entries = [ended[0], ended[1], (&after, AT + 2 * HOUR, false)];
        assert_eq!(resolve(&name, &log(&name, &entries)), Err(Error::Log(3)));
    }
    // The higher key undoes it.
    let back = sign(operation(&[&rotation(2)], &[("zu", key(1))], &[], Some(&first)), &rotation(2));
    let entries = [ended[0], (&end, AT + HOUR, true), (&back, AT + 2 * HOUR, false)];
    assert_eq!(resolve(&name, &log(&name, &entries)).unwrap().keys, vec![(key(1), None)]);
    // A tombstone follows something, and has no other field.
    let bare = sign(json!({ "type": "plc_tombstone", "prev": null }), &rotation(1));
    assert_eq!(resolve(&did(&bare), &log(&did(&bare), &[(&bare, AT, false)])), Err(Error::Log(1)));
    let more = sign(with(tombstone(&first), "alsoKnownAs", json!([])), &rotation(1));
    assert_eq!(
        resolve(&name, &log(&name, &[ended[0], (&more, AT + HOUR, false)])),
        Err(Error::Log(2))
    );
}

/// did-zu.md § Operation Serialization: the legacy creation is checked in its own format.
/// Its rotation keys are the recovery key, then the signing key.
#[test]
fn a_legacy_creation_is_a_genesis_operation() {
    let create = |signer: u8| {
        let op = json!({
            "type": "create",
            "signingKey": rotation(1).did_key,
            "recoveryKey": rotation(2).did_key,
            "handle": "someone.example.com",
            "service": "https://host.example.com",
            "prev": null,
        });
        sign(op, &rotation(signer))
    };
    for signer in [1, 2] {
        let first = create(signer);
        let name = did(&first);
        let empty =
            Doc { did: name.clone(), keys: vec![], handles: vec![], services: BTreeMap::new() };
        assert_eq!(resolve(&name, &log(&name, &[(&first, AT, false)])), Ok(empty));
        // The recovery key comes first: it undoes what the signing key did, not the reverse.
        let low = sign(operation(&[&rotation(1)], &[], &[], Some(&first)), &rotation(1));
        let high = sign(operation(&[&rotation(2)], &[], &[], Some(&first)), &rotation(2));
        let entries = [(&first, AT, false), (&low, AT + 1, true), (&high, AT + 2, false)];
        assert!(resolve(&name, &log(&name, &entries)).is_ok());
        let entries = [(&first, AT, false), (&high, AT + 1, true), (&low, AT + 2, false)];
        assert_eq!(resolve(&name, &log(&name, &entries)), Err(Error::Log(3)));
    }
    let stranger = create(3);
    assert_eq!(
        resolve(&did(&stranger), &log(&did(&stranger), &[(&stranger, AT, false)])),
        Err(Error::Log(1))
    );
    // It is only ever the first operation, and it has exactly its own fields.
    let first = genesis(&[], &[]);
    let later = sign(with(create(1), "prev", json!(cid(&first))), &rotation(1));
    assert_eq!(
        resolve(&did(&first), &log(&did(&first), &[(&first, AT, false), (&later, AT + 1, false)])),
        Err(Error::Log(2))
    );
    for (field, to) in [("handle", json!(null)), ("service", json!(1)), ("services", json!({}))] {
        let odd = sign(with(create(1), field, to), &rotation(1));
        assert_eq!(
            resolve(&did(&odd), &log(&did(&odd), &[(&odd, AT, false)])),
            Err(Error::Log(1)),
            "{field}"
        );
    }
}

/// did-zu.md § Operation Serialization: an operation has the fields of its type, each of its
/// type, and no others. What it is made of is what its signature and its CID are over.
#[test]
fn an_operation_has_exactly_the_fields_of_its_type() {
    let good = operation(&[&rotation(1)], &[("zu", key(1))], &["zu://a.4seas.example"], None);
    let made = |op: Json| {
        let op = sign(op, &rotation(1));
        resolve(&did(&op), &log(&did(&op), &[(&op, AT, false)]))
    };
    assert!(made(good.clone()).is_ok());
    let service =
        json!({ "relay": { "type": "IrohRelay", "endpoint": "https://relay.example.com" } });
    let relay = ("IrohRelay".to_owned(), "https://relay.example.com".to_owned());
    let stated = made(with(good.clone(), "services", service)).unwrap().services;
    assert_eq!(stated, BTreeMap::from([("relay".to_owned(), relay)]));
    let changes = [
        ("type", json!("plc_op")),
        ("type", json!(null)),
        ("rotationKeys", json!("did:key:zQ3sh")),
        ("rotationKeys", json!([1])),
        ("rotationKeys", json!([null])),
        ("verificationMethods", json!([])),
        ("verificationMethods", json!({ "zu": 1 })),
        ("verificationMethods", json!({ "zu": null })),
        ("alsoKnownAs", json!({})),
        ("alsoKnownAs", json!(["zu://a.4seas.example", 2])),
        ("services", json!([])),
        ("services", json!({ "relay": "https://relay.example.com" })),
        ("services", json!({ "relay": { "type": "IrohRelay" } })),
        ("services", json!({ "relay": { "type": "IrohRelay", "endpoint": 1 } })),
        (
            "services",
            json!({ "relay": { "type": "IrohRelay", "endpoint": "https://e.example.com", "x": "y" } }),
        ),
        ("prev", json!(false)),
        ("extra", json!("field")),
        ("extra", json!(null)),
        ("extra", json!(5)),
        ("extra", json!(true)),
    ];
    for (field, to) in changes {
        assert_eq!(
            made(with(good.clone(), field, to.clone())),
            Err(Error::Log(1)),
            "{field}: {to}"
        );
    }
    for field in ["type", "rotationKeys", "verificationMethods", "alsoKnownAs", "services", "prev"]
    {
        let mut less = good.clone();
        less.as_object_mut().unwrap().remove(field);
        assert_eq!(made(less), Err(Error::Log(1)), "{field}");
    }
    // Without a signature, or with one that is no string.
    let name = did(&good);
    assert_eq!(resolve(&name, &log(&name, &[(&good, AT, false)])), Err(Error::Log(1)));
    let unsigned = with(good.clone(), "sig", json!(null));
    assert_eq!(
        resolve(&did(&unsigned), &log(&did(&unsigned), &[(&unsigned, AT, false)])),
        Err(Error::Log(1))
    );
    // An entry that is no entry.
    for entry in [json!(null), json!([]), json!({}), json!({ "operation": "op" })] {
        let entries = serde_json::to_vec(&json!([entry])).unwrap();
        assert_eq!(resolve(&name, &entries), Err(Error::Log(1)), "{entry}");
    }
}

/// A name of a map that the JSON form of the data model reserves is an ordinary name in an
/// operation, which is DAG-CBOR and has neither links nor bytes.
#[test]
fn an_operation_is_read_as_dag_cbor_and_not_as_the_json_of_the_data_model() {
    let methods = json!({ "$link": format!("did:key:{}", key(1)) });
    let op = sign(
        with(operation(&[&rotation(1)], &[], &[], None), "verificationMethods", methods),
        &rotation(1),
    );
    let doc = resolve(&did(&op), &log(&did(&op), &[(&op, AT, false)])).unwrap();
    assert_eq!(doc.keys, vec![]);
    let services = json!({ "$bytes": { "type": "AAAA", "endpoint": "AAAA" } });
    let op =
        sign(with(operation(&[&rotation(1)], &[], &[], None), "services", services), &rotation(1));
    let doc = resolve(&did(&op), &log(&did(&op), &[(&op, AT, false)])).unwrap();
    assert_eq!(doc.services.keys().collect::<Vec<_>>(), ["$bytes"]);
}

/// did.md § DID Documents, for an operation: the Zu signing keys are the Ed25519 keys under
/// the names for such keys; the handles are the valid `zu://` entries, the primary one first.
#[test]
fn an_operation_states_zu_signing_keys_and_handles() {
    let named = [
        ("zu", key(1)),
        ("zu-7xq2k4mf", key(2)),
        ("atproto", key(3)),
        ("zu-short", key(4)),
        ("zu2", key(5)),
    ];
    let aka = [
        "at://someone.test",
        "zu://Someone.4seas.example",
        "zu://someone.zuzalu.example",
        "zu://org",
        "zu://a.co/x",
    ];
    let op = genesis(&named, &aka);
    let doc = resolve(&did(&op), &log(&did(&op), &[(&op, AT, false)])).unwrap();
    assert_eq!(doc.keys, vec![(key(1), None), (key(2), None)]);
    assert_eq!(doc.handles, ["someone.4seas.example", "someone.zuzalu.example"]);
    // A key of another kind under the name `zu` is no Zu signing key. It breaks nothing.
    let methods = json!({ "zu": rotation(1).did_key, "zu-7xq2k4mf": "did:key:z6Mk", "zu-aaaaaaaa": key(6).to_string() });
    let op = sign(
        with(operation(&[&rotation(1)], &[], &[], None), "verificationMethods", methods),
        &rotation(1),
    );
    assert_eq!(resolve(&did(&op), &log(&did(&op), &[(&op, AT, false)])).unwrap().keys, vec![]);
}

/// record.md § Keys and the DID Document: a key that was removed still counts for a record
/// whose `at` is not later than the time at which the directory recorded the removal.
#[test]
fn a_removed_key_counts_until_its_removal_was_recorded() {
    let (name, ops) = chain(&[
        &[("zu", key(1))],
        &[("zu", key(1)), ("zu-7xq2k4mf", key(2))],
        &[("zu-7xq2k4mf", key(2))],
    ]);
    let doc = resolve(&name, &hourly(&name, &ops)).unwrap();
    let removed = AT + 2 * HOUR;
    assert_eq!(doc.keys, vec![(key(1), Some(removed)), (key(2), None)]);
    assert!(doc.key_now(&key(2)) && !doc.key_now(&key(1)) && !doc.key_now(&key(3)));
    // Up to the removal, and not a millisecond later.
    assert!(doc.key_at(&key(1), removed) && doc.key_at(&key(1), removed - 1));
    assert!(!doc.key_at(&key(1), removed + 1) && !doc.key_at(&key(1), i64::MAX));
    // `at` is the signer's own statement and may be in the past: nothing asks that the key
    // was already there. A current key counts whatever the time.
    assert!(doc.key_at(&key(1), AT - HOUR) && doc.key_at(&key(1), i64::MIN));
    assert!(doc.key_at(&key(2), AT - HOUR) && doc.key_at(&key(2), i64::MAX));
    assert!(!doc.key_at(&key(3), AT));
}

#[test]
fn a_key_that_came_back_is_current_and_one_removed_twice_counts_until_the_last_removal() {
    let with_key: &[(&str, PublicKey)] = &[("zu", key(1))];
    let (name, ops) = chain(&[with_key, &[], with_key]);
    let doc = resolve(&name, &hourly(&name, &ops)).unwrap();
    assert_eq!(doc.keys, vec![(key(1), None)]);
    assert!(doc.key_at(&key(1), AT + HOUR + 1));
    let (name, ops) = chain(&[with_key, &[], with_key, &[], &[]]);
    let doc = resolve(&name, &hourly(&name, &ops)).unwrap();
    assert_eq!(doc.keys, vec![(key(1), Some(AT + 3 * HOUR))]);
}

/// The document is that of the last operation standing.
#[test]
fn handles_and_services_are_those_of_the_latest_operation() {
    let first = genesis(&[], &["zu://old.4seas.example"]);
    let second = sign(
        operation(
            &[&rotation(1)],
            &[],
            &["zu://new.4seas.example", "zu://old.4seas.example"],
            Some(&first),
        ),
        &rotation(1),
    );
    let name = did(&first);
    let doc =
        resolve(&name, &log(&name, &[(&first, AT, false), (&second, AT + 1, false)])).unwrap();
    assert_eq!(doc.handles, ["new.4seas.example", "old.4seas.example"]);
}

/// did-zu.md § Bulk Export: `createdAt` is `YYYY-MM-DDTHH:mm:ss.sssZ`.
#[test]
fn the_time_of_an_operation_is_read_from_its_one_format() {
    let with_key: &[(&str, PublicKey)] = &[("zu", key(1))];
    let (name, ops) = chain(&[with_key, &[]]);
    let removed_at = |first: &str, second: &str| {
        let mut json: Json = serde_json::from_slice(&hourly(&name, &ops)).unwrap();
        (json[0]["createdAt"], json[1]["createdAt"]) = (json!(first), json!(second));
        resolve(&name, &serde_json::to_vec(&json).unwrap()).map(|doc| doc.keys[0].1)
    };
    let epoch = "1970-01-01T00:00:00.000Z";
    let known = [
        ("1970-01-01T00:00:00.001Z", 1),
        ("1970-01-02T00:00:00.000Z", 86_400_000),
        ("2000-02-29T12:34:56.789Z", 951_827_696_789),
        ("2000-03-01T00:00:00.000Z", 951_868_800_000),
        ("2026-11-13T00:00:00.000Z", AT),
        ("2100-03-01T00:00:00.000Z", 4_107_542_400_000),
        ("9999-12-31T23:59:59.999Z", 253_402_300_799_999),
    ];
    for (text, at) in known {
        assert_eq!(removed_at(epoch, text), Ok(Some(at)), "{text}");
        assert_eq!(stamp(at), text);
    }
    assert_eq!(removed_at("0000-01-01T00:00:00.000Z", epoch), Ok(Some(0)));
    let refused = [
        "2026-11-13T00:00:00Z",
        "2026-11-13T00:00:00.000",
        "2026-11-13T00:00:00.000z",
        "2026-11-13 00:00:00.000Z",
        "2026-11-13T00:00:00.000+00:00",
        "2026-11-13T00:00:00.0000Z",
        "2026-11-13T00:00:00,000Z",
        "2026-13-01T00:00:00.000Z",
        "2026-00-10T00:00:00.000Z",
        "2026-11-00T00:00:00.000Z",
        "2026-11-31T00:00:00.000Z",
        "2026-02-29T00:00:00.000Z",
        "2100-02-29T00:00:00.000Z",
        "2026-11-13T24:00:00.000Z",
        "2026-11-13T00:60:00.000Z",
        "2026-11-13T00:00:60.000Z",
        "+026-11-13T00:00:00.000Z",
        "２０２６-11-13T00:00:00.000Z",
        "",
    ];
    for text in refused {
        assert_eq!(removed_at(epoch, text), Err(Error::Log(2)), "{text}");
    }
    let mut json: Json = serde_json::from_slice(&hourly(&name, &ops)).unwrap();
    json[1]["createdAt"] = json!(AT + HOUR);
    assert_eq!(resolve(&name, &serde_json::to_vec(&json).unwrap()), Err(Error::Log(2)));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// The time of an operation reads back as the time it was written from, for every
    /// millisecond from 1970 to the end of the year 9999.
    #[test]
    fn any_time_reads_back(at in 1i64..253_402_300_800_000) {
        let with_key: &[(&str, PublicKey)] = &[("zu", key(1))];
        let (name, ops) = chain(&[with_key, &[]]);
        let doc = resolve(&name, &log(&name, &[(&ops[0], 0, false), (&ops[1], at, false)])).unwrap();
        prop_assert_eq!(doc.keys, vec![(key(1), Some(at))]);
    }

    /// Whatever is given as a log or as a document, the answer is an answer.
    #[test]
    fn any_bytes_are_refused_without_a_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200), text in ".{0,80}") {
        let _ = resolve(DID, &bytes);
        let _ = resolve(WEB, &bytes);
        let entry = json!([{ "did": DID, "operation": { "type": text, "sig": text, "prev": text }, "cid": text, "createdAt": text }]);
        prop_assert!(resolve(DID, &serde_json::to_vec(&entry).unwrap()).is_err());
        let _ = (url(&text, true), origin(&text, true), handle(&text), is_did(&text));
    }
}

/// A rotation key signs its own name too: the helper makes what the kernel reads.
#[test]
fn the_helpers_make_keys_of_both_curves() {
    let keys: Vec<Rotation> = (1..=4).map(rotation).collect();
    assert!(
        keys[0].did_key.starts_with("did:key:zQ3s") && keys[2].did_key.starts_with("did:key:zQ3s")
    );
    assert!(
        keys[1].did_key.starts_with("did:key:zDn") && keys[3].did_key.starts_with("did:key:zDn")
    );
}

/// did-zu.md § DID Creation, with a P-256 rotation key made from these bytes.
fn made(n: u8, signing: u8) -> (zu::did::Rotation, String, Json) {
    let rotation = zu::did::Rotation::from_bytes(&[n; 32]).unwrap();
    let (did, op) = zu::did::create(&rotation, &key(signing)).unwrap();
    (rotation, did, serde_json::from_str(&op).unwrap())
}

/// The spec has no vector for a creation, so what is made here is read back by the rules every
/// log is checked by: a key of its own signed it, in low-S form, and it names the DID.
#[test]
fn a_did_made_here_is_one_the_kernel_resolves_to_the_key_it_was_made_with() {
    // About half the signatures come out high-S before they are made low: twenty keys see both.
    for n in 1..=20 {
        let (_, did, op) = made(n, 1);
        assert_eq!(did, oplog::did(&op), "the DID is the start of the hash, as the spec writes it");
        let doc = resolve(&did, &log(&did, &[(&op, AT, false)])).unwrap();
        assert_eq!(doc.keys, [(key(1), None)]);
        assert!(doc.handles.is_empty() && doc.services.is_empty());
        assert!(serde_json::to_vec(&op).unwrap().len() < 7500);
    }
}

/// did.md § DID Documents: the one Zu signing key, under `zu-` and eight characters drawn from
/// the key; one P-256 rotation key, as the test helper writes the same key; nothing else.
#[test]
fn a_did_made_here_states_one_rotation_key_one_signing_key_and_no_handle() {
    use sha2::{Digest, Sha256};
    let (own, did, op) = made(2, 3);
    let name = &multibase::Base::Base32Lower.encode(Sha256::digest(key(3).as_bytes()))[..8];
    let expected = json!({
        "type": "plc_operation",
        "rotationKeys": [rotation(2).did_key],
        "verificationMethods": { format!("zu-{name}"): format!("did:key:{}", key(3)) },
        "alsoKnownAs": [],
        "services": {},
        "prev": null,
        "sig": op["sig"],
    });
    assert_eq!(op, expected);
    assert_eq!(own.did_key(), rotation(2).did_key);
    assert!(own.did_key().starts_with("did:key:zDnae"));
    // The same keys make the same DID; another signing key, another DID.
    assert_eq!(made(2, 3).1, did);
    assert_ne!(made(2, 4).1, did);
}

/// A rotation key is a scalar of P-256: zero, and anything not below the group's order, are not.
#[test]
fn a_rotation_key_is_made_only_from_a_scalar() {
    assert!(zu::did::Rotation::from_bytes(&[0; 32]).is_err());
    assert!(zu::did::Rotation::from_bytes(&[0xff; 32]).is_err());
    let mut order: [u8; 32] =
        common::hex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551")
            .try_into()
            .unwrap();
    assert!(zu::did::Rotation::from_bytes(&order).is_err());
    order[31] -= 1;
    assert!(zu::did::Rotation::from_bytes(&order).is_ok());
}

#[test]
fn a_rotation_key_is_written_out_as_a_p256_private_multikey() {
    let secret = [7; 32];
    let written = zu::did::Rotation::from_bytes(&secret).unwrap().multikey();
    let bytes = multibase::Base::Base58Btc.decode(&written[1..]).unwrap();
    assert_eq!((&written[..1], &bytes[..2], &bytes[2..]), ("z", &[0x86, 0x26][..], &secret[..]));
}

#[test]
fn a_devices_own_identity_is_the_did_key_of_its_signing_key() {
    let key = key(1);
    let local = zu::did::local(&key);
    assert_eq!(local, format!("did:key:{key}"));
    assert_eq!(zu::did::local_key(&local), Some(key));

    let p256 = zu::did::Rotation::from_bytes(&[7; 32]).unwrap().did_key();
    let upper = local.replacen("did:", "DID:", 1);
    let extra = format!("{local}a");
    let refused = [
        DID,
        WEB,
        "did:key:zQ3shZc2QzApp2oymGvQbzP8eKheVshBHbU4ZYjeXqwSKEn6N",
        &p256,
        "did:key:z6Mk",
        &upper,
        &extra,
    ];
    for did in refused {
        assert_eq!(zu::did::local_key(did), None, "{did}");
    }

    assert!(!is_did(&local));
    assert_eq!(url(&local, false), Err(Error::Method));
    assert_eq!(url(&local, true), Err(Error::Method));
    assert_eq!(resolve(&local, b"{}"), Err(Error::Method));
}
