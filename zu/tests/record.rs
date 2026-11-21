//! record.md: the header, signing, verifying, and which record a path refers to.

mod common;

use common::{AT, CITY, DID, cid, fields, header, hex, map, path, record, seal, secret, text};
use zu::record::{current, resolve};
use zu::{Cid, Error, PublicKey, Record, Value};

fn anyone(_: &str, _: &PublicKey, _: i64) -> bool {
    true
}

fn nothing_held(_: &Cid) -> Option<i64> {
    None
}

/// record.md § Header: the example of the specification, with its elided values filled in.
#[test]
fn the_header_has_exactly_the_fields_of_the_specification() {
    let prev = cid("an earlier record");
    let signed = Record::sign(
        &secret(1),
        path("kx7q2m5v4tbn3hpw"),
        Some(cid("body")),
        [prev],
        AT,
        Some(CITY),
    )
    .unwrap();
    let expected = map([
        ("$type", text("record")),
        ("v", Value::Int(1)),
        ("by", text("did:zu:ewvi7nxzyoun6zhxrhs64oiz")),
        ("to", text("did:zu:q4cm5j2xk7hvn3oaz6t2wplb")),
        ("type", text("ev/event")),
        ("key", text("kx7q2m5v4tbn3hpw")),
        ("at", Value::Int(1794528000000)),
        ("prev", Value::List(vec![Value::Link(prev)])),
        ("body", Value::Link(cid("body"))),
        ("pub", text(&secret(1).public().to_string())),
        ("sig", Value::Bytes(signed.sig.to_vec())),
    ]);
    assert_eq!(signed.header(), expected);
    assert_eq!(signed.verify(anyone, nothing_held), Ok(()));
    // § Signing: the signature is over the header without `sig`, in DRISL.
    let mut unsigned = fields(expected);
    unsigned.remove("sig");
    assert_eq!(secret(1).public().verify(&Value::Map(unsigned).encode(), &signed.sig), Ok(()));
    // In DRISL the keys come shortest first: v, at, by, to, key, pub, sig, body, prev, type, $type.
    let bytes = signed.header().encode();
    assert_eq!(bytes[..8], [0xab, 0x61, b'v', 0x01, 0x62, b'a', b't', 0x1b]);
    // A record without a city has no `to` field, and one that removes has a null body.
    let removal =
        Record::sign(&secret(1), path("kx7q2m5v4tbn3hpw"), None, [signed.cid()], AT + 1, None)
            .unwrap();
    let removal = fields(removal.header());
    assert_eq!(removal.len(), 10);
    assert_eq!((removal.get("to"), removal.get("body")), (None, Some(&Value::Null)));
}

/// A fixed record, to notice any change in the bytes this kernel signs. Ed25519 and DRISL
/// are both deterministic, so every implementation arrives at the same values; the CID, the
/// signature and the multikey below were also computed apart from this crate, with Python's
/// hashlib and OpenSSL. The specifications give no such values yet.
#[test]
fn a_fixed_record() {
    let prev = [cid("b"), cid("a")];
    let signed =
        Record::sign(&secret(1), path("kx7q2m5v4tbn3hpw"), Some(cid("body")), prev, AT, Some(CITY))
            .unwrap();
    let json = concat!(
        r#"{"$type":"record","at":1794528000000,"#,
        r#""body":{"$link":"bafyreibdbwbvrxeorciljrmn523csexof4qdk6xjfjomqynzrzup4mnmwu"},"#,
        r#""by":"did:zu:ewvi7nxzyoun6zhxrhs64oiz","key":"kx7q2m5v4tbn3hpw","#,
        r#""prev":[{"$link":"bafyreib6epubmabzlffdhckpmvsodmjuro6xuaei2qwevs3t52xnlhaatu"},"#,
        r#"{"$link":"bafyreigks6arfsq3xxfpvqrrwonchxcnu6do76auprhhfomao6c273sixm"}],"#,
        r#""pub":"z6Mkon3Necd6NkkyfoGoHxid2znGc59LU3K7mubaRcFbLfLX","#,
        r#""sig":{"$bytes":"20K0Z8y2b4o96pp5sYMbaBw4gP1xKhT0B7bP8c+0bO0VfmQsD3W7kr5/9lzLwbW0tRnIcrS9BigXYyzKOMruCA"},"#,
        r#""to":"did:zu:q4cm5j2xk7hvn3oaz6t2wplb","type":"ev/event","v":1}"#,
    );
    let drisl = "
        ab 6176 01 626174 1b000001a1d2343800
        626279 781f 6469643a7a753a65777669376e787a796f756e367a687872687336346f697a
        62746f 781f 6469643a7a753a7134636d356a32786b3768766e336f617a36743277706c62
        636b6579 70 6b783771326d35763474626e33687077
        63707562 7830 7a364d6b6f6e334e656364364e6b6b79666f476f48786964327a6e476335394c55334b376d756261526346624c664c58
        63736967 5840 db42b467ccb66f8a3dea9a79b1831b681c3880fd712a14f407b6cff1cfb46ced
                      157e642c0f75bb92be7ff65ccbc1b5b4b519c872b4bd062817632cca38caee08
        64626f6479 d82a 5825 00 01711220 230d8358dc8e8890b4c58deeb62912ee2f20357ae92a5cc861b98e68fe31acb5
        6470726576 82
            d82a 5825 00 01711220 3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d
            d82a 5825 00 01711220 ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb
        6474797065 68 65762f6576656e74
        652474797065 66 7265636f7264";
    assert_eq!(signed.header().to_json().unwrap(), json);
    assert_eq!(signed.header().encode(), hex(drisl));
    assert_eq!(
        signed.cid().to_string(),
        "bafyreihyzx44ov6zf4t3gs26kam6x5ppdxvy2el6j5jux7yinhswwpovkm"
    );
    // And from the text back to the record.
    let read = Record::try_from(Value::from_json(json).unwrap()).unwrap();
    assert_eq!(read, signed);
    assert_eq!(Record::try_from(Value::decode(&hex(drisl)).unwrap()), Ok(read));
}

/// § Header: the CID of a record is the CID of its header in DRISL, and the header reads
/// back from JSON text to the same bytes.
#[test]
fn bytes_json_and_cid_agree() {
    let signed = record("a", AT, &[]);
    let bytes = signed.header().encode();
    assert_eq!(signed.cid(), Cid::of(zu::Codec::Drisl, &bytes));
    assert_eq!(Record::try_from(Value::decode(&bytes).unwrap()), Ok(signed.clone()));
    let json = signed.header().to_json().unwrap();
    let read = Record::try_from(Value::from_json(&json).unwrap()).unwrap();
    assert_eq!(read, signed);
    assert_eq!(read.header().encode(), bytes);
    assert_eq!(read.verify(anyone, nothing_held), Ok(()));
}

#[test]
fn prev_is_sorted_and_without_duplicates() {
    let (a, b, c) = (cid("a"), cid("b"), cid("c"));
    let signed = Record::sign(&secret(1), path("k"), None, [c, a, b, a, c], AT, None).unwrap();
    let mut sorted = vec![a, b, c];
    sorted.sort_by_key(|cid| *cid.as_bytes());
    assert_eq!(signed.prev, sorted);
    // A header whose `prev` is out of order or repeats itself is not a header, however
    // well it is signed.
    let with_prev = |prev: Vec<Cid>| {
        let mut header = header("k");
        header.insert("prev".to_owned(), Value::List(prev.into_iter().map(Value::Link).collect()));
        Record::try_from(seal(&secret(1), Value::Map(header)))
    };
    assert!(with_prev(sorted.clone()).is_ok());
    assert_eq!(with_prev(vec![sorted[1], sorted[0]]), Err(Error::Field("prev")));
    assert_eq!(with_prev(vec![sorted[0], sorted[2], sorted[1]]), Err(Error::Field("prev")));
    assert_eq!(with_prev(vec![sorted[0], sorted[0]]), Err(Error::Field("prev")));
    assert_eq!(with_prev(vec![sorted[0], sorted[1], sorted[1]]), Err(Error::Field("prev")));
}

/// § Verifying a Record: every required field, each of the right type and syntax.
#[test]
fn refuses_headers_of_the_wrong_structure() {
    let sealed = |header| Record::try_from(seal(&secret(1), Value::Map(header)));
    assert!(sealed(header("k")).is_ok());
    for name in ["$type", "v", "by", "type", "key", "at", "prev", "body"] {
        let mut header = header("k");
        header.remove(name);
        assert_eq!(sealed(header), Err(Error::Field(name)), "without {name}");
    }
    let wrong = [
        ("$type", text("auth"), Error::Field("$type")),
        ("$type", text("Record"), Error::Field("$type")),
        ("$type", Value::Int(1), Error::Field("$type")),
        ("v", Value::Int(2), Error::Field("v")),
        ("v", Value::Int(0), Error::Field("v")),
        ("v", text("1"), Error::Field("v")),
        ("by", Value::Int(1), Error::Field("by")),
        ("by", text("did:plc:ewvi7nxzyoun6zhxrhs64oiz"), Error::Path),
        ("by", text("someone.4seas.example"), Error::Path),
        ("to", Value::Null, Error::Field("to")),
        (
            "to",
            text("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"),
            Error::Field("to"),
        ),
        ("to", text(""), Error::Field("to")),
        ("type", text("event"), Error::Path),
        ("type", text("ev/*"), Error::Path),
        ("type", Value::Null, Error::Field("type")),
        ("key", text("a/b"), Error::Path),
        ("key", text(".."), Error::Path),
        ("key", Value::Int(7), Error::Field("key")),
        ("at", text("1794528000000"), Error::Field("at")),
        ("at", Value::Null, Error::Field("at")),
        ("prev", Value::Null, Error::Field("prev")),
        ("prev", Value::Link(cid("a")), Error::Field("prev")),
        ("prev", Value::List(vec![text(&cid("a").to_string())]), Error::Field("prev")),
        ("body", text(&cid("a").to_string()), Error::Field("body")),
        ("body", Value::Bool(false), Error::Field("body")),
        ("body", map([]), Error::Field("body")),
    ];
    for (name, value, error) in wrong {
        let mut header = header("k");
        header.insert(name.to_owned(), value.clone());
        assert_eq!(sealed(header), Err(error), "{name} = {value:?}");
    }
    // `pub` and `sig`.
    let signed = fields(record("k", AT, &[]).header());
    let wrong = [
        ("pub", Value::Null, Error::Field("pub")),
        ("pub", Value::Bytes(secret(1).public().as_bytes().to_vec()), Error::Field("pub")),
        ("pub", text("z6Mk"), Error::Key),
        ("pub", text(&format!("did:key:{}", secret(1).public())), Error::Key),
        ("sig", Value::Null, Error::Field("sig")),
        ("sig", Value::Bytes(vec![0; 63]), Error::Field("sig")),
        ("sig", Value::Bytes(vec![0; 65]), Error::Field("sig")),
        ("sig", text("AAAA"), Error::Field("sig")),
    ];
    for (name, value, error) in wrong {
        let mut header = signed.clone();
        header.insert(name.to_owned(), value.clone());
        assert_eq!(Record::try_from(Value::Map(header)), Err(error), "{name} = {value:?}");
        let mut header = signed.clone();
        header.remove(name);
        assert_eq!(Record::try_from(Value::Map(header)), Err(Error::Field(name)), "without {name}");
    }
    for value in [Value::Null, Value::List(vec![]), text("record")] {
        assert_eq!(Record::try_from(value), Err(Error::Field("$type")));
    }
    // What `sign` is given goes through the same checks.
    assert_eq!(
        Record::sign(&secret(1), path("k"), None, [], AT, Some("pol.4seas.example")),
        Err(Error::Field("to"))
    );
    let mut bad = path("k");
    bad.key = "a/b".to_owned();
    assert_eq!(Record::sign(&secret(1), bad, None, [], AT, None), Err(Error::Path));
}

/// § Header: `by`, `type`, `key` and `to` are inside the signed header, so a record cannot
/// be presented under another path or for another city. Neither can anything else change.
#[test]
fn a_changed_header_no_longer_verifies() {
    let signed =
        Record::sign(&secret(1), path("k"), Some(cid("body")), [cid("a")], AT, Some(CITY)).unwrap();
    assert_eq!(signed.verify(anyone, nothing_held), Ok(()));
    let other = "did:zu:aaaaaaaaaaaaaaaaaaaaaaaa";
    type Change = fn(&mut Record);
    let changes: [(&str, Change); 12] = [
        ("by", |r| r.path.by = "did:zu:aaaaaaaaaaaaaaaaaaaaaaaa".to_owned()),
        ("type", |r| r.path.ty = "ev/rsvp".to_owned()),
        ("key", |r| r.path.key = "j".to_owned()),
        ("to another city", |r| r.to = Some("did:zu:aaaaaaaaaaaaaaaaaaaaaaaa".to_owned())),
        ("to no city", |r| r.to = None),
        ("at", |r| r.at += 1),
        ("prev", |r| r.prev.clear()),
        ("body", |r| r.body = Some(cid("another body"))),
        ("body removed", |r| r.body = None),
        ("pub", |r| r.pubkey = secret(2).public()),
        ("sig", |r| r.sig[0] ^= 1),
        ("an added field", |r| drop(r.rest.insert("x".to_owned(), Value::Int(1)))),
    ];
    for (what, change) in changes {
        let mut changed = signed.clone();
        change(&mut changed);
        assert_eq!(changed.verify(anyone, nothing_held), Err(Error::Signature), "{what}");
        // The same change made to the header as it travels: it still parses, and fails.
        let changed = Record::try_from(Value::decode(&changed.header().encode()).unwrap()).unwrap();
        assert_eq!(changed.verify(anyone, nothing_held), Err(Error::Signature), "{what}");
    }
    assert_ne!(other, DID);
    // A header signed as another kind of signed data is not a record (cryptography.md
    // § Signing Data), and a record's signature does not carry over to it.
    let mut as_auth = fields(signed.header());
    as_auth.insert("$type".to_owned(), text("auth"));
    assert_eq!(Record::try_from(Value::Map(as_auth)), Err(Error::Field("$type")));
}

/// § Header: software ignores header fields it does not know, and keeps them, because they
/// are part of the signed bytes.
#[test]
fn unknown_fields_are_kept() {
    let mut header = header("k");
    header.insert("size".to_owned(), Value::Int(120));
    header.insert("$future".to_owned(), map([("deep", Value::List(vec![Value::Null]))]));
    let sealed = seal(&secret(1), Value::Map(header));
    let read = Record::try_from(sealed.clone()).unwrap();
    assert_eq!(read.verify(anyone, nothing_held), Ok(()));
    assert_eq!(read.header(), sealed);
    assert_eq!(read.cid(), Cid::of(zu::Codec::Drisl, &sealed.encode()));
    assert_eq!(read.rest.keys().collect::<Vec<_>>(), ["$future", "size"]);
    // Through JSON text and back, the unknown fields are still there and still signed.
    let json = read.header().to_json().unwrap();
    assert_eq!(
        Record::try_from(Value::from_json(&json).unwrap()).unwrap().header().encode(),
        sealed.encode()
    );
    // Dropping one breaks the signature.
    let mut dropped = read.clone();
    dropped.rest.remove("size");
    assert_eq!(dropped.verify(anyone, nothing_held), Err(Error::Signature));
}

/// § Keys and the DID Document: whether `pub` is a key of `by` is the caller's to answer.
#[test]
fn the_key_must_be_a_key_of_the_individual() {
    let signed = record("k", AT, &[]);
    let mut asked = None;
    let known = secret(1).public();
    assert_eq!(
        signed.verify(
            |did, key, at| asked.insert((did.to_owned(), *key, at)).1 == known,
            nothing_held
        ),
        Ok(())
    );
    assert_eq!(asked, Some((DID.to_owned(), known, AT)));
    assert_eq!(
        signed.verify(|_, key, _| *key == secret(2).public(), nothing_held),
        Err(Error::Signer)
    );
    // The signature comes first: a forged header never reaches the question.
    let mut forged = signed.clone();
    forged.at += 1;
    assert_eq!(
        forged.verify(|_, _, _| panic!("asked about a forged header"), nothing_held),
        Err(Error::Signature)
    );
}

/// § Time: `at` is greater than the `at` of every record listed in `prev` that is held.
#[test]
fn at_must_be_greater_than_at_of_prev() {
    let first = record("k", AT, &[]);
    let at_of = |held: &[Record]| {
        let held = held.to_vec();
        move |cid: &Cid| held.iter().find(|r| r.cid() == *cid).map(|r| r.at)
    };
    for (at, verdict) in [
        (AT + 1, Ok(())),
        (AT, Err(Error::Time)),
        (AT - 1, Err(Error::Time)),
        (0, Err(Error::Time)),
    ] {
        let next = record("k", at, &[&first]);
        assert_eq!(next.verify(anyone, at_of(std::slice::from_ref(&first))), verdict, "at = {at}");
        // A record whose `prev` is not held cannot be checked against it, and is not refused.
        assert_eq!(next.verify(anyone, nothing_held), Ok(()));
    }
    // One listed record that is later is enough.
    let (early, late) = (record("a", AT - 10, &[]), record("b", AT + 10, &[]));
    let both = record("k", AT, &[&early, &late]);
    assert_eq!(both.verify(anyone, at_of(&[early.clone(), late.clone()])), Err(Error::Time));
    assert_eq!(both.verify(anyone, at_of(&[early])), Ok(()));
    // Times before 1970 are integers like any other.
    let before = record("k", -5, &[]);
    assert_eq!(before.verify(anyone, nothing_held), Ok(()));
    assert_eq!(record("k", -4, &[&before]).verify(anyone, at_of(&[before])), Ok(()));
}

/// § Which Record a Path Refers To.
#[test]
fn a_path_with_no_record_refers_to_none() {
    assert_eq!(resolve(&[], &path("k")), None);
    assert_eq!(resolve(&[record("other", AT, &[])], &path("k")), None);
    assert!(current(&[record("other", AT, &[])], &path("k")).is_empty());
}

#[test]
fn in_a_chain_the_path_refers_to_the_last_record() {
    let a = record("k", AT, &[]);
    let b = record("k", AT + 1, &[&a]);
    let c = record("k", AT + 2, &[&b]);
    for held in [[&a, &b, &c], [&c, &a, &b], [&b, &c, &a]] {
        let held: Vec<Record> = held.into_iter().cloned().collect();
        assert_eq!(resolve(&held, &path("k")), Some(&c));
        assert_eq!(current(&held, &path("k")), [&c]);
    }
    // Held twice, a record is still one record.
    assert_eq!(current(&[c.clone(), b.clone(), c.clone(), b.clone()], &path("k")), [&c]);
    // Without the middle record nothing shows that `c` follows `a`: both are current.
    assert_eq!(current(&[a.clone(), c.clone()], &path("k")), [&c, &a]);
}

/// Time never decides between two records of which one follows the other (§ Time).
#[test]
fn prev_decides_before_time() {
    let late = record("k", AT + 1000, &[]);
    let honest =
        Record::sign(&secret(1), path("k"), Some(cid("x")), [late.cid()], AT + 1001, None).unwrap();
    assert_eq!(resolve(&[late.clone(), honest.clone()], &path("k")), Some(&honest));
    // Even a follower with an earlier `at` replaces: it is the caller that keeps invalid
    // records out of `held` (`Record::verify`), not this rule.
    let earlier =
        Record::sign(&secret(1), path("k"), Some(cid("y")), [late.cid()], AT, None).unwrap();
    assert_eq!(resolve(&[late, earlier.clone()], &path("k")), Some(&earlier));
}

#[test]
fn concurrent_records_the_greatest_at_wins_then_the_greatest_cid() {
    let root = record("k", AT, &[]);
    let (x, y) = (record("k", AT + 5, &[&root]), record("k", AT + 9, &[&root]));
    assert_eq!(current(&[root.clone(), x.clone(), y.clone()], &path("k")), [&y, &x]);
    assert_eq!(resolve(&[y.clone(), x.clone(), root.clone()], &path("k")), Some(&y));
    // Equal `at`: the greatest CID, compared byte by byte.
    let mut tied: Vec<Record> = (0..8)
        .map(|n| {
            Record::sign(
                &secret(1),
                path("k"),
                Some(cid(&format!("body {n}"))),
                [root.cid()],
                AT + 5,
                None,
            )
            .unwrap()
        })
        .collect();
    let greatest = tied.iter().max_by_key(|r| *r.cid().as_bytes()).unwrap().clone();
    assert_eq!(resolve(&tied, &path("k")), Some(&greatest));
    tied.reverse();
    assert_eq!(resolve(&tied, &path("k")), Some(&greatest));
    let order: Vec<Cid> = current(&tied, &path("k")).iter().map(|r| r.cid()).collect();
    assert!(order.windows(2).all(|pair| pair[0].as_bytes() > pair[1].as_bytes()));
    // The next record lists all current ones, and is then the only current one.
    let merged = Record::sign(
        &secret(1),
        path("k"),
        Some(cid("merged")),
        current(&tied, &path("k")).iter().map(|r| r.cid()),
        AT + 6,
        None,
    )
    .unwrap();
    tied.push(merged.clone());
    assert_eq!(current(&tied, &path("k")), [&merged]);
}

#[test]
fn a_removed_path_refers_to_no_record() {
    let a = record("k", AT, &[]);
    let removal = Record::sign(&secret(1), path("k"), None, [a.cid()], AT + 1, None).unwrap();
    let held = vec![a.clone(), removal.clone()];
    assert_eq!(resolve(&held, &path("k")), None);
    assert_eq!(current(&held, &path("k")), [&removal], "the removing record is itself current");
    // Written again after the removal.
    let again = record("k", AT + 2, &[&removal]);
    assert_eq!(resolve(&[held, vec![again.clone()]].concat(), &path("k")), Some(&again));
    // A removal concurrent with an update: the later of the two decides.
    let update = record("k", AT + 3, &[&a]);
    assert_eq!(resolve(&[a.clone(), removal.clone(), update.clone()], &path("k")), Some(&update));
    let late_removal = Record::sign(&secret(1), path("k"), None, [a.cid()], AT + 4, None).unwrap();
    assert_eq!(resolve(&[a, update, late_removal], &path("k")), None);
}

/// § Earlier Records: B follows A if B lists A directly or through other records, also
/// through records of other paths and of other individuals.
#[test]
fn following_goes_through_records_of_other_paths() {
    let a = record("k", AT, &[]);
    let elsewhere = Record::sign(
        &secret(2),
        zu::Path::new(CITY, "forum/post", "x").unwrap(),
        Some(cid("post")),
        [a.cid()],
        AT + 1,
        None,
    )
    .unwrap();
    let b = record("k", AT + 2, &[&elsewhere]);
    assert_eq!(current(&[a.clone(), elsewhere.clone(), b.clone()], &path("k")), [&b]);
    assert_eq!(current(&[a.clone(), b.clone()], &path("k")), [&b, &a], "the link is not held");
    // Records of the same key under another type or another individual are other paths.
    let other_type = Record::sign(
        &secret(1),
        zu::Path::new(DID, "ev/rsvp", "k").unwrap(),
        Some(cid("r")),
        [a.cid()],
        AT + 9,
        None,
    )
    .unwrap();
    let other_did = Record::sign(
        &secret(2),
        zu::Path::new(CITY, "ev/event", "k").unwrap(),
        Some(cid("c")),
        [],
        AT + 9,
        None,
    )
    .unwrap();
    assert_eq!(resolve(&[a.clone(), other_type, other_did], &path("k")), Some(&a));
}
