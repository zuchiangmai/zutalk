//! query.md: the query, its filter, the answer, and what is discarded from an answer.

mod common;

use common::{AT, CITY, DID, fields, header, map, path, record, seal, secret, text};
use proptest::prelude::*;
use std::collections::BTreeMap;
use zu::query::answer;
use zu::{Cid, Codec, Error, Held, Path, PublicKey, Query, Record, Value};

const HOST: &str = "https://pol.4seas.example";
const OTHER: &str = "did:zu:aaaaaaaaaaaaaaaaaaaaaaaa";

/// A record of `by`, of this type and key, meant for `to`, with this body.
fn signed(
    by: &str,
    ty: &str,
    key: &str,
    at: i64,
    to: Option<&str>,
    body: Option<&Value>,
) -> Record {
    let body = body.map(|body| Cid::of(Codec::Drisl, &body.encode()));
    Record::sign(&secret(1), Path::new(by, ty, key).unwrap(), body, [], at, to).unwrap()
}

fn body(name: &str) -> Value {
    map([("name", text(name))])
}

fn held(records: &[Record], bodies: &[Value]) -> Held {
    let bodies = bodies.iter().map(|body| (Cid::of(Codec::Drisl, &body.encode()), body.clone()));
    Held { records: records.to_vec(), bodies: bodies.collect() }
}

/// An answer written by hand, as a host may write it.
fn written<const N: usize>(fields: [(&str, Value); N]) -> Vec<u8> {
    map(fields).encode()
}

fn headers(records: &[&Record]) -> Value {
    Value::List(records.iter().map(|record| record.header()).collect())
}

/// § The Query: the four filters that cover the common cases.
#[test]
fn the_common_filters_are_written_as_the_parameters_of_a_get() {
    let one = Query::one(&path("kx7q2m5v4tbn3hpw"));
    assert_eq!(
        one.url(HOST).as_deref(),
        Ok(
            "https://pol.4seas.example/.well-known/zu-recs?by=did:zu:ewvi7nxzyoun6zhxrhs64oiz&type=ev/event&key=kx7q2m5v4tbn3hpw"
        )
    );
    let of = Query::of(DID, "ev/event");
    assert_eq!(
        of.url(HOST).as_deref(),
        Ok(
            "https://pol.4seas.example/.well-known/zu-recs?by=did:zu:ewvi7nxzyoun6zhxrhs64oiz&type=ev/event"
        )
    );
    let city = Query::across("ev/event", Some(CITY));
    assert_eq!(
        city.url(HOST).as_deref(),
        Ok(
            "https://pol.4seas.example/.well-known/zu-recs?to=did:zu:q4cm5j2xk7hvn3oaz6t2wplb&type=ev/event"
        )
    );
    let everyone = Query::across("ev/*", None);
    assert_eq!(
        everyone.url(HOST).as_deref(),
        Ok("https://pol.4seas.example/.well-known/zu-recs?type=ev/*")
    );
}

/// § The Query: every parameter is optional. record.md § Delivering: records are given to a
/// host at the address that answers queries.
#[test]
fn every_parameter_is_optional_and_each_is_written_once() {
    assert_eq!(
        Query::default().url(HOST).as_deref(),
        Ok("https://pol.4seas.example/.well-known/zu-recs")
    );
    let all = Query {
        by: Some(DID.into()),
        to: Some(CITY.into()),
        ty: Some("ev/event".into()),
        key: Some("a.b-c_d:e~f".into()),
        since: Some(AT),
        until: Some(AT + 1),
        n: Some(50),
        from: Some("opaque".into()),
        all: true,
    };
    assert_eq!(
        all.url("http://127.0.0.1:8080").as_deref(),
        Ok("http://127.0.0.1:8080/.well-known/zu-recs?by=did:zu:ewvi7nxzyoun6zhxrhs64oiz\
            &to=did:zu:q4cm5j2xk7hvn3oaz6t2wplb&type=ev/event&key=a.b-c_d:e~f\
            &since=1794528000000&until=1794528000001&n=50&from=opaque&all=1")
    );
    let times = Query { since: Some(-1), until: Some(0), n: Some(0), ..Query::default() };
    assert_eq!(times.url(HOST).unwrap().split_once('?').unwrap().1, "since=-1&until=0&n=0");
}

/// What a parameter holds reaches the host as it is: `next` is opaque, and a `did:web` may
/// hold a percent sign.
#[test]
fn a_value_that_a_query_string_would_change_is_percent_encoded() {
    let from = Query { from: Some("a b&c=d#e?f%g+h/é\n".into()), ..Query::default() };
    assert_eq!(
        from.url(HOST).unwrap().split_once('?').unwrap().1,
        "from=a%20b%26c%3Dd%23e%3Ff%25g%2Bh/%C3%A9%0A"
    );
    let web = Query::of("did:web:localhost%3A8080", "ev/event");
    assert_eq!(
        web.url(HOST).unwrap().split_once('?').unwrap().1,
        "by=did:web:localhost%253A8080&type=ev/event"
    );
}

/// § The Query: `by` and `to` are DIDs, `type` is a type or a class with a star, `key` a key.
#[test]
fn a_parameter_of_the_wrong_syntax_is_not_asked() {
    let wrong = [
        Query::of("someone.4seas.example", "ev/event"),
        Query::of("did:plc:ewvi7nxzyoun6zhxrhs64oiz", "ev/event"),
        Query::of(DID, "event"),
        Query::of(DID, "ev/"),
        Query::of(DID, "Ev/event"),
        Query::of(DID, "ev/*x"),
        Query::of(DID, "ev/**"),
        Query::of(DID, "*/event"),
        Query::of(DID, "*"),
        Query::of(DID, "ev/event/more"),
        Query::of(DID, ""),
        Query::across("ev/event", Some("someone.4seas.example")),
        Query::across("ev/event", Some("")),
        Query { key: Some("a/b".into()), ..Query::default() },
        Query { key: Some("..".into()), ..Query::default() },
        Query { key: Some(String::new()), ..Query::default() },
    ];
    for query in wrong {
        assert_eq!(query.url(HOST), Err(Error::Path), "{query:?}");
    }
    for ty in ["ev/event", "ev/*", "a/*", "forum-2/post"] {
        assert!(Query::across(ty, None).url(HOST).is_ok(), "{ty}");
    }
}

/// § The Query: a record matches the filter when it matches every parameter given.
#[test]
fn a_record_matches_when_it_matches_every_parameter_given() {
    let event = signed(DID, "ev/event", "one", AT, Some(CITY), None);
    assert!(Query::default().matches(&event));
    assert!(Query::one(&event.path).matches(&event));
    assert!(Query::of(DID, "ev/event").matches(&event));
    assert!(Query::across("ev/event", Some(CITY)).matches(&event));
    assert!(Query::across("ev/event", None).matches(&event));
    // One parameter that does not match is enough.
    assert!(!Query::of(OTHER, "ev/event").matches(&event));
    assert!(!Query::of(DID, "ev/note").matches(&event));
    assert!(!Query::across("ev/event", Some(OTHER)).matches(&event));
    assert!(!Query::one(&Path::new(DID, "ev/event", "two").unwrap()).matches(&event));
    assert!(!Query { key: Some("One".into()), ..Query::default() }.matches(&event));
    // `to` selects records whose header `to` is that DID: one without `to` has none.
    let profile = signed(DID, "ev/event", "one", AT, None, None);
    assert!(!Query::across("ev/event", Some(CITY)).matches(&profile));
    assert!(Query::across("ev/event", None).matches(&profile));
}

/// § The Query: `ev/*` is any type of the class `ev`.
#[test]
fn a_star_stands_for_every_kind_of_one_class() {
    let of = |ty: &str| signed(DID, ty, "one", AT, None, None);
    let class = Query::across("ev/*", None);
    assert!(
        class.matches(&of("ev/event"))
            && class.matches(&of("ev/note"))
            && class.matches(&of("ev/e"))
    );
    assert!(
        !class.matches(&of("eve/event"))
            && !class.matches(&of("e/event"))
            && !class.matches(&of("forum/ev"))
    );
    // A star anywhere else is no class: such a filter is not asked, and matches nothing.
    for ty in ["ev*", "e*", "*", "ev/e*", "*/event"] {
        let odd = Query { ty: Some(ty.into()), ..Query::default() };
        assert!(!odd.matches(&of("ev/event")) && !odd.matches(&of("eve/event")), "{ty}");
        assert_eq!(odd.url(HOST), Err(Error::Path), "{ty}");
    }
}

/// § The Query: `since` is this time or later, `until` is earlier than this time.
#[test]
fn since_includes_its_time_and_until_does_not() {
    let at = |at: i64| signed(DID, "ev/event", "one", at, None, None);
    let span = Query { since: Some(AT), until: Some(AT + 10), ..Query::default() };
    assert!(span.matches(&at(AT)) && span.matches(&at(AT + 9)));
    assert!(
        !span.matches(&at(AT - 1)) && !span.matches(&at(AT + 10)) && !span.matches(&at(AT + 11))
    );
    let empty = Query { since: Some(AT), until: Some(AT), ..Query::default() };
    assert!(!empty.matches(&at(AT)));
}

/// § The Answer: `recs`, the headers as they were signed; `blocks`, the bodies under the
/// string form of their CIDs; `next`, when more records match.
#[test]
fn an_answer_has_the_fields_of_the_specification() {
    let (first, second) = (body("first"), body("second"));
    let records = [
        signed(DID, "ev/event", "one", AT, None, Some(&first)),
        signed(DID, "ev/event", "two", AT + 1, None, Some(&second)),
    ];
    let bytes = answer(&held(&records, &[first.clone(), second.clone()]), Some("more"));
    let expected = map([
        ("recs", headers(&[&records[1], &records[0]])),
        (
            "blocks",
            map([
                (records[0].body.unwrap().to_string().as_str(), first.clone()),
                (records[1].body.unwrap().to_string().as_str(), second.clone()),
            ]),
        ),
        ("next", text("more")),
    ]);
    assert_eq!(Value::decode(&bytes), Ok(expected));
    let (read, next) = Query::of(DID, "ev/event").read(&bytes).unwrap();
    assert_eq!(read, held(&[records[1].clone(), records[0].clone()], &[first, second]));
    assert_eq!(next.as_deref(), Some("more"));
    // record.md § Delivering: the request has the same fields, and no `next`.
    let delivery = fields(Value::decode(&answer(&held(&records, &[]), None)).unwrap());
    assert_eq!(delivery.keys().collect::<Vec<_>>(), ["blocks", "recs"]);
    // `blocks` and `next` are optional.
    let (read, next) =
        Query::default().read(&written([("recs", headers(&[&records[0]]))])).unwrap();
    assert_eq!((read, next), (held(&records[..1], &[]), None));
    assert_eq!(
        Query::default().read(&written([("recs", Value::List(vec![]))])),
        Ok((Held::default(), None))
    );
}

/// § The Answer: the headers are ordered by `at`, latest first; headers with the same `at`
/// by the binary form of their CIDs, greatest first.
#[test]
fn records_come_the_latest_first_and_then_the_greatest_cid_first() {
    let mut same: Vec<Record> = ["a", "b", "c", "d"]
        .iter()
        .map(|key| signed(DID, "ev/event", key, AT, None, None))
        .collect();
    same.sort_by_key(|record| std::cmp::Reverse(*record.cid().as_bytes()));
    let (earlier, later) = (record("early", AT - 1, &[]), record("late", AT + 1, &[]));
    let expected: Vec<Record> = [vec![later.clone()], same.clone(), vec![earlier.clone()]].concat();
    // However the host ordered them, and however the writer was handed them.
    let shuffled = [&same[2], &earlier, &same[0], &later, &same[3], &same[1]];
    let (read, _) = Query::default().read(&written([("recs", headers(&shuffled))])).unwrap();
    assert_eq!(read.records, expected);
    let by_the_kernel = answer(&held(&shuffled.map(Clone::clone), &[]), None);
    assert_eq!(
        fields(Value::decode(&by_the_kernel).unwrap())["recs"],
        headers(&expected.iter().collect::<Vec<_>>())
    );
    // A header sent twice is one record.
    let twice = written([("recs", headers(&[&later, &earlier, &later]))]);
    assert_eq!(Query::default().read(&twice).unwrap().0.records, [later, earlier]);
}

/// § The Answer: a header that fails its check is discarded, and the rest is kept.
#[test]
fn a_header_that_is_no_header_is_discarded() {
    let good = record("good", AT, &[]);
    let mut no_key = header("x");
    no_key.remove("key");
    let mut bad_type = header("x");
    bad_type.insert("type".into(), text("Event"));
    let not_headers = vec![
        good.header(),
        seal(&secret(1), Value::Map(no_key)),
        seal(&secret(1), Value::Map(bad_type)),
        body("a body"),
        Value::Null,
        Value::Int(1),
        text("record"),
        Value::List(vec![good.header()]),
    ];
    let (read, _) = Query::default().read(&written([("recs", Value::List(not_headers))])).unwrap();
    assert_eq!(read.records, [good]);
}

/// The host is not trusted to have applied the filter: what does not match is discarded.
#[test]
fn a_record_that_was_not_asked_for_is_discarded() {
    let asked = signed(DID, "ev/event", "one", AT, None, None);
    let others = [
        signed(OTHER, "ev/event", "one", AT, None, None),
        signed(DID, "ev/note", "one", AT, None, None),
        signed(DID, "ev/event", "two", AT, None, None),
    ];
    let sent = written([("recs", headers(&[&others[0], &asked, &others[1], &others[2]]))]);
    assert_eq!(Query::one(&asked.path).read(&sent).unwrap().0.records, vec![asked.clone()]);
    assert_eq!(Query::of(DID, "ev/event").read(&sent).unwrap().0.records.len(), 2);
    assert_eq!(Query::default().read(&sent).unwrap().0.records.len(), 4);
}

/// § The Answer, record.md § Body: every body is checked against the `body` CID of its
/// header. A body that fails is discarded, and so is one that no header links to.
#[test]
fn a_body_is_kept_only_under_the_cid_by_which_a_record_links_to_it() {
    let (wanted, unasked) = (body("wanted"), body("not asked for"));
    let with_body = signed(DID, "ev/event", "one", AT, None, Some(&wanted));
    let removal = signed(DID, "ev/event", "two", AT, None, None);
    let name = |body: &Value| Cid::of(Codec::Drisl, &body.encode()).to_string();
    let recs = headers(&[&with_body, &removal]);
    let read = |blocks: Value| {
        Query::default().read(&written([("recs", recs.clone()), ("blocks", blocks)])).unwrap().0
    };
    let good = map([(name(&wanted).as_str(), wanted.clone())]);
    assert_eq!(read(good).bodies, BTreeMap::from([(with_body.body.unwrap(), wanted.clone())]));
    let discarded = [
        // Another body under the CID that the record links to.
        map([(name(&wanted).as_str(), unasked.clone())]),
        // The right body under another name: a CID that is not its own, or no CID at all.
        map([(name(&unasked).as_str(), wanted.clone())]),
        map([("body", wanted.clone())]),
        map([(name(&wanted).to_uppercase().as_str(), wanted.clone())]),
        map([(Cid::of(Codec::Raw, &wanted.encode()).to_string().as_str(), wanted.clone())]),
        // A body that no record of the answer links to.
        map([(name(&unasked).as_str(), unasked.clone())]),
    ];
    for blocks in discarded {
        assert_eq!(read(blocks.clone()).bodies, BTreeMap::new(), "{blocks:?}");
    }
    // record.md § Body: a body is a map. Anything else is no body, even under the CID that a
    // record links to.
    for not_a_map in [text("wanted"), Value::List(vec![wanted.clone()]), Value::Null] {
        let linking = signed(DID, "ev/event", "three", AT, None, Some(&not_a_map));
        let blocks = map([(name(&not_a_map).as_str(), not_a_map.clone())]);
        let sent = written([("recs", headers(&[&linking])), ("blocks", blocks)]);
        let (read, _) = Query::default().read(&sent).unwrap();
        assert_eq!((read.records, read.bodies), (vec![linking], BTreeMap::new()), "{not_a_map:?}");
    }
    // A body whose header was discarded goes with it.
    let other = Query::of(OTHER, "ev/event");
    let sent =
        written([("recs", recs.clone()), ("blocks", map([(name(&wanted).as_str(), wanted)]))]);
    assert_eq!(other.read(&sent), Ok((Held::default(), None)));
}

/// § The Answer: nothing in an answer is trusted because the host said it. Reading it
/// checks each header for what it is; whose it is, is still to be verified.
#[test]
fn a_record_read_from_an_answer_is_not_yet_verified() {
    let good = record("good", AT, &[]);
    let mut forged = good.clone();
    forged.at += 1;
    let sent = written([("recs", headers(&[&good, &forged]))]);
    let records = Query::default().read(&sent).unwrap().0.records;
    assert_eq!(records, [forged.clone(), good.clone()]);
    let key_of_did = |did: &str, key: &PublicKey, _: i64| did == DID && *key == secret(1).public();
    assert_eq!(good.verify(key_of_did, |_| None), Ok(()));
    assert_eq!(forged.verify(key_of_did, |_| None), Err(Error::Signature));
    assert_eq!(good.verify(|_, _, _| false, |_| None), Err(Error::Signer));
}

/// § The Answer: a map in DRISL, with `recs` a list, `blocks` a map and `next` a string.
#[test]
fn an_answer_of_another_shape_is_refused() {
    let query = Query::default();
    let recs = ("recs", Value::List(vec![]));
    let refused = [
        (b"not drisl".to_vec(), Error::Drisl),
        (br#"{"recs":[]}"#.to_vec(), Error::Drisl),
        (Vec::new(), Error::Drisl),
        (Value::List(vec![]).encode(), Error::Field("recs")),
        (written([]), Error::Field("recs")),
        (written([("recs", map([]))]), Error::Field("recs")),
        (written([("recs", Value::Null)]), Error::Field("recs")),
        (written([("blocks", map([]))]), Error::Field("recs")),
        (written([recs.clone(), ("blocks", Value::List(vec![]))]), Error::Field("blocks")),
        (written([recs.clone(), ("blocks", Value::Null)]), Error::Field("blocks")),
        (written([recs.clone(), ("next", Value::Int(1))]), Error::Field("next")),
        (written([recs.clone(), ("next", Value::Null)]), Error::Field("next")),
    ];
    for (bytes, error) in refused {
        assert_eq!(query.read(&bytes), Err(error));
    }
    // A field that is not known is ignored, so that the answer can grow.
    let more = written([recs, ("stat", map([])), ("next", text(""))]);
    assert_eq!(query.read(&more), Ok((Held::default(), Some(String::new()))));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// What a host writes for a filter, read with that filter, is every record that matches
    /// it and no other, in the order of the specification, each body under its record.
    #[test]
    fn an_answer_reads_back_as_the_records_that_match(
        picks in proptest::collection::vec((0usize..2, 0usize..3, 0usize..3, 0i64..4, any::<bool>()), 0..12),
        by in proptest::option::of(0usize..2),
        ty in proptest::option::of(0usize..4),
        since in proptest::option::of(0i64..4),
        until in proptest::option::of(0i64..5),
    ) {
        let dids = [DID, OTHER];
        let types = ["ev/event", "ev/note", "forum/post", "ev/*"];
        let mut records = Vec::new();
        let mut bodies = Vec::new();
        for (who, ty, key, at, to) in picks {
            let body = body(&format!("{who}{ty}{key}{at}{to}"));
            records.push(signed(dids[who], types[ty], &format!("k{key}"), AT + at, to.then_some(CITY), Some(&body)));
            bodies.push(body);
        }
        let query = Query {
            by: by.map(|i| dids[i].to_owned()),
            ty: ty.map(|i| types[i].to_owned()),
            since: since.map(|at| AT + at),
            until: until.map(|at| AT + at),
            ..Query::default()
        };
        prop_assert!(query.url(HOST).is_ok());
        let (read, next) = query.read(&answer(&held(&records, &bodies), None)).unwrap();
        prop_assert_eq!(next, None);
        let mut expected: Vec<Record> = records.iter().filter(|record| query.matches(record)).cloned().collect();
        expected.sort_by_key(|record| std::cmp::Reverse((record.at, record.cid())));
        expected.dedup();
        prop_assert_eq!(&read.records, &expected);
        let linked: Vec<Cid> = expected.iter().filter_map(|record| record.body).collect();
        prop_assert!(read.bodies.keys().all(|cid| linked.contains(cid)));
        prop_assert!(linked.iter().all(|cid| read.bodies.contains_key(cid)));
        prop_assert!(read.bodies.iter().all(|(cid, body)| *cid == Cid::of(Codec::Drisl, &body.encode())));
    }

    #[test]
    fn any_bytes_are_refused_without_a_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200), text in ".{0,60}") {
        let _ = Query::default().read(&bytes);
        let query = Query { by: Some(text.clone()), ty: Some(text.clone()), key: Some(text.clone()), from: Some(text), ..Query::default() };
        let _ = query.url(HOST);
    }
}
