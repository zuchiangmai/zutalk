//! data-model.md: DRISL bytes, the strict gate, and the JSON representation.

mod common;

use common::{hex, map, text};
use ipld_core::ipld::Ipld;
use proptest::prelude::*;
use zu::{Cid, Codec, Error, Value};

fn list<const N: usize>(items: [Value; N]) -> Value {
    Value::List(items.to_vec())
}

/// Each value with the one DRISL encoding it has.
fn vectors() -> Vec<(Value, &'static str)> {
    vec![
        (Value::Null, "f6"),
        (Value::Bool(false), "f4"),
        (Value::Bool(true), "f5"),
        (Value::Int(0), "00"),
        (Value::Int(23), "17"),
        (Value::Int(24), "1818"),
        (Value::Int(255), "18ff"),
        (Value::Int(256), "190100"),
        (Value::Int(65535), "19ffff"),
        (Value::Int(65536), "1a00010000"),
        (Value::Int(4294967295), "1affffffff"),
        (Value::Int(4294967296), "1b0000000100000000"),
        (Value::Int(1794528000000), "1b000001a1d2343800"),
        (Value::Int(i64::MAX), "1b7fffffffffffffff"),
        // Examples of RFC 8949, appendix A.
        (Value::Int(1000000), "1a000f4240"),
        (Value::Int(1000000000000), "1b000000e8d4a51000"),
        (Value::Int(-1000), "3903e7"),
        (text("IETF"), "6449455446"),
        (text("\u{00fc}"), "62c3bc"),
        (text("\u{6c34}"), "63e6b0b4"),
        (Value::Bytes(vec![1, 2, 3, 4]), "4401020304"),
        (
            list([
                Value::Int(1),
                list([Value::Int(2), Value::Int(3)]),
                list([Value::Int(4), Value::Int(5)]),
            ]),
            "8301820203820405",
        ),
        (
            map([("a", Value::Int(1)), ("b", list([Value::Int(2), Value::Int(3)]))]),
            "a26161016162820203",
        ),
        (Value::Int(-1), "20"),
        (Value::Int(-24), "37"),
        (Value::Int(-25), "3818"),
        (Value::Int(-256), "38ff"),
        (Value::Int(-257), "390100"),
        (Value::Int(i64::MIN), "3b7fffffffffffffff"),
        (text(""), "60"),
        (text("a"), "6161"),
        (text("é"), "62c3a9"),
        (text("abcdefghijklmnopqrstuvwx"), "7818 6162636465666768696a6b6c6d6e6f707172737475767778"),
        (Value::Bytes(vec![]), "40"),
        (Value::Bytes(vec![1, 2, 3]), "43010203"),
        (list([]), "80"),
        (list([Value::Int(1), list([Value::Null])]), "820181f6"),
        (map([]), "a0"),
        (map([("a", Value::Int(1)), ("b", Value::Int(2))]), "a2616101616202"),
        // The shorter key first, whatever the alphabet says.
        (map([("aa", Value::Int(1)), ("b", Value::Int(2))]), "a2616202626161 01"),
        (map([("$type", text("blob")), ("v", Value::Int(1))]), "a2617601652474797065 64626c6f62"),
    ]
}

#[test]
fn encodes_to_the_one_drisl_form() {
    for (value, bytes) in vectors() {
        assert_eq!(value.encode(), hex(bytes), "{value:?}");
        assert_eq!(Value::decode(&hex(bytes)), Ok(value));
    }
}

#[test]
fn a_link_is_tag_42_over_a_zero_byte_and_the_binary_cid() {
    let cid = Cid::of(Codec::Raw, b"");
    let bytes = Value::Link(cid).encode();
    assert_eq!(bytes[..5], [0xd8, 0x2a, 0x58, 0x25, 0x00]);
    assert_eq!(bytes[5..], cid.as_bytes()[..]);
    assert_eq!(Value::decode(&bytes), Ok(Value::Link(cid)));
}

/// Every strictness rule, as bytes that some CBOR decoder would take.
#[test]
fn refuses_what_is_not_drisl() {
    let refused = [
        ("", "empty input"),
        ("0000", "two values"),
        ("f6f6", "trailing bytes"),
        ("1800", "integer not in its shortest form"),
        ("1817", "integer not in its shortest form"),
        ("1900ff", "integer not in its shortest form"),
        ("1a0000ffff", "integer not in its shortest form"),
        ("1b00000000ffffffff", "integer not in its shortest form"),
        ("3800", "negative integer not in its shortest form"),
        ("390018", "negative integer not in its shortest form"),
        ("5800", "byte string length not in its shortest form"),
        ("590001ff", "byte string length not in its shortest form"),
        ("780161", "text length not in its shortest form"),
        ("7a0000000161", "text length not in its shortest form"),
        ("9800", "list length not in its shortest form"),
        ("b800", "map length not in its shortest form"),
        ("1b8000000000000000", "integer above 64 bits with a sign"),
        ("1bffffffffffffffff", "integer above 64 bits with a sign"),
        ("3b8000000000000000", "integer below 64 bits with a sign"),
        ("3bffffffffffffffff", "integer below 64 bits with a sign"),
        ("c249010000000000000000", "big integer"),
        ("f93c00", "half float"),
        ("fa3f800000", "single float"),
        ("fb3ff0000000000000", "double float"),
        ("fb7ff8000000000000", "NaN"),
        ("fb7ff0000000000000", "infinity"),
        ("fb0000000000000000", "float zero"),
        ("f7", "undefined"),
        ("e0", "simple value"),
        ("f820", "simple value"),
        ("ff", "break"),
        ("5f4100ff", "indefinite byte string"),
        ("7f6161ff", "indefinite text"),
        ("9f01ff", "indefinite list"),
        ("bf616101ff", "indefinite map"),
        ("a2616101616102", "duplicate map key"),
        ("a2616201616102", "map keys out of order"),
        ("a2626161016162 02", "longer map key first"),
        ("a10102", "integer as map key"),
        ("a1410102", "bytes as map key"),
        ("a1f601", "null as map key"),
        ("62c328", "text that is not UTF-8"),
        ("61ff", "text that is not UTF-8"),
        ("c000", "tag 0"),
        ("c11a514b67b0", "tag 1"),
        ("d82b00", "tag 43"),
        ("d82a00", "tag 42 on an integer"),
        ("d82a6161", "tag 42 on text"),
        (
            "d9002a582500 0171 1220 0000000000000000000000000000000000000000000000000000000000000000",
            "tag 42 not in its shortest form",
        ),
        ("6261", "text cut short"),
        ("8201", "list cut short"),
        ("a16161", "map cut short"),
        ("18", "head cut short"),
    ];
    for (bytes, why) in refused {
        assert!(Value::decode(&hex(bytes)).is_err(), "{why}: {bytes}");
    }
}

/// data-model.md § Link and CID Formats, for links inside DRISL.
#[test]
fn refuses_links_outside_the_allowed_set() {
    let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let link = |cid: &str| hex(&format!("d82a58{:02x}{cid}", hex(cid).len()));
    assert!(Value::decode(&link(&format!("00 01 71 12 20 {digest}"))).is_ok());
    assert!(Value::decode(&link(&format!("00 01 55 12 20 {digest}"))).is_ok());
    let refused = [
        (format!("01 71 12 20 {digest}"), "no leading zero byte"),
        (format!("00 01 70 12 20 {digest}"), "codec dag-pb"),
        (format!("00 01 0129 12 20 {digest}"), "codec dag-json"),
        (format!("00 12 20 {digest}"), "CID version 0"),
        (format!("00 02 71 12 20 {digest}"), "CID version 2"),
        (format!("00 01 71 13 40 {digest}{digest}"), "sha-512"),
        (format!("00 01 71 1e 20 {digest}"), "blake3"),
        (format!("00 01 71 12 1f {}", &digest[2..]), "hash of 31 bytes"),
        (format!("00 01 71 12 20 {digest}00"), "a byte too many"),
        (format!("00 01 f100 12 20 {digest}"), "codec not in its shortest form"),
        ("00 01 71 00 00".to_owned(), "identity hash"),
        ("00".to_owned(), "empty CID"),
        (String::new(), "empty byte string"),
    ];
    for (cid, why) in refused {
        assert!(Value::decode(&link(&cid)).is_err(), "{why}");
    }
}

#[test]
fn nesting_too_deep_is_refused_not_a_crash() {
    for depth in [300, 100_000] {
        let mut bytes = vec![0x81; depth];
        bytes.push(0x00);
        assert_eq!(Value::decode(&bytes), Err(Error::Drisl));
        assert_eq!(Value::from_json(&("[".repeat(depth) + &"]".repeat(depth))), Err(Error::Json));
    }
    // A list that claims more items than there are bytes.
    assert_eq!(Value::decode(&hex("9b00ffffffffffffff")), Err(Error::Drisl));
    assert_eq!(Value::decode(&hex("5b00ffffffffffffff")), Err(Error::Drisl));
}

/// data-model.md § JSON Representation: the two examples of the specification.
#[test]
fn json_examples_of_the_specification() {
    let link = r#"{"src":{"$link":"bafyreidfayvfuwqa7qlnopdjiqrxzs6blmoeu4rujcjtnci5beludirz2a"}}"#;
    let value = Value::from_json(link).unwrap();
    let cid: Cid = "bafyreidfayvfuwqa7qlnopdjiqrxzs6blmoeu4rujcjtnci5beludirz2a".parse().unwrap();
    assert_eq!(value, map([("src", Value::Link(cid))]));
    assert_eq!(value.to_json().unwrap(), link);
    assert_eq!(value.encode()[..10], hex("a1 63737263 d82a 5825 00"));

    let bytes = r#"{"salt":{"$bytes":"nFERjvLLiw9qm45JrqH9QTzyC2Lu1Xb4ne6+sBrCzI0"}}"#;
    let salt = hex("9c51118ef2cb8b0f6a9b8e49aea1fd413cf20b62eed576f89deebeb01ac2cc8d");
    let value = Value::from_json(bytes).unwrap();
    assert_eq!(value, map([("salt", Value::Bytes(salt.clone()))]));
    assert_eq!(value.to_json().unwrap(), bytes);
    // `=` padding is optional, and spacing and key order are free.
    let padded = "{ \"salt\" : {\n\"$bytes\": \"nFERjvLLiw9qm45JrqH9QTzyC2Lu1Xb4ne6+sBrCzI0=\" } }";
    assert_eq!(Value::from_json(padded).unwrap(), value);
}

#[test]
fn json_maps_every_type() {
    let value = map([
        ("null", Value::Null),
        ("yes", Value::Bool(true)),
        ("min", Value::Int(i64::MIN)),
        ("max", Value::Int(i64::MAX)),
        ("text", text("line\nbreak \"quoted\" \u{2028} é")),
        ("bytes", Value::Bytes(vec![0xfb, 0xff, 0xfe])),
        ("empty", Value::Bytes(vec![])),
        ("list", list([Value::Int(1), map([])])),
        ("$type", text("blob")),
    ]);
    let json = value.to_json().unwrap();
    assert!(!json.contains('\n'), "one line: {json}");
    assert!(json.contains(r#""bytes":{"$bytes":"+//+"}"#), "plain base64, not URL-safe: {json}");
    assert_eq!(Value::from_json(&json).unwrap(), value);
    assert_eq!(Value::from_json(&json).unwrap().encode(), value.encode());
}

#[test]
fn json_refuses_what_the_data_model_cannot_hold() {
    let refused = [
        "",
        "{",
        "1 2",
        "1.5",
        "1.0",
        "1e3",
        "-0.0",
        "9223372036854775808",
        "-9223372036854775809",
        "NaN",
        r#"{"$bytes":true}"#,
        r#"{"$bytes":"-_-_"}"#,
        r#"{"$bytes":"AA="}"#,
        r#"{"$bytes":"AQ==","more":1}"#,
        r#"{"$bytes":"AR"}"#,
        r#"{"$link":5}"#,
        r#"{"$link":"bafy"}"#,
        r#"{"$link":"QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG"}"#,
        r#"{"$link":"bafyreidfayvfuwqa7qlnopdjiqrxzs6blmoeu4rujcjtnci5beludirz2a","$bytes":"AQ"}"#,
        r#"{"$link":"bafyreidfayvfuwqa7qlnopdjiqrxzs6blmoeu4rujcjtnci5beludirz2a","more":1}"#,
    ];
    for json in refused {
        assert!(Value::from_json(json).is_err(), "{json}");
    }
    assert_eq!(Value::from_json(r#"{"$bytes":"AQ=="}"#), Ok(Value::Bytes(vec![1])));
    assert_eq!(Value::from_json(r#"{"$bytes":"AQ"}"#), Ok(Value::Bytes(vec![1])));
}

/// A map that uses the reserved names of the JSON representation has no JSON text: written
/// out, it would read back as a link or as bytes, with other DRISL bytes.
#[test]
fn json_refuses_to_write_what_would_not_read_back() {
    let cid = Cid::of(Codec::Drisl, b"");
    let reserved = [
        map([("$link", text(&cid.to_string()))]),
        map([("$link", Value::Int(1))]),
        map([("$bytes", text("AQ"))]),
        map([("$bytes", Value::Null), ("more", Value::Int(1))]),
        list([map([("deep", map([("$link", text(&cid.to_string()))]))])]),
    ];
    for value in reserved {
        assert_eq!(Value::decode(&value.encode()).as_ref(), Ok(&value), "it is DRISL");
        assert_eq!(value.to_json(), Err(Error::Json), "{value:?}");
    }
    // The deepest nesting that is read from DRISL: its JSON text is one level deeper when a
    // link or bytes are at the bottom, and that is deeper than JSON text is read.
    let nested = |leaf: Value| (0..127).fold(leaf, |inner, _| list([inner]));
    for (leaf, json) in
        [(Value::Null, true), (Value::Bytes(vec![1]), false), (Value::Link(cid), false)]
    {
        let deep = nested(leaf);
        assert_eq!(Value::decode(&deep.encode()).as_ref(), Ok(&deep));
        assert_eq!(deep.to_json().is_ok(), json);
        assert_eq!(Value::decode(&list([deep]).encode()), Err(Error::Drisl));
    }
}

fn values() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        prop_oneof![Just(0), Just(23), Just(24), Just(-24), Just(-25), Just(255), Just(256)]
            .prop_map(Value::Int),
        ".{0,40}".prop_map(Value::Str),
        proptest::collection::vec(any::<u8>(), 0..300).prop_map(Value::Bytes),
        (any::<bool>(), any::<Vec<u8>>()).prop_map(|(raw, content)| Value::Link(Cid::of(
            if raw { Codec::Raw } else { Codec::Drisl },
            &content
        ))),
    ];
    leaf.prop_recursive(5, 64, 8, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..8).prop_map(Value::List),
            proptest::collection::btree_map(".{0,12}", inner, 0..8).prop_map(Value::Map),
        ]
    })
}

/// The same value in the library's own type, to compare our encoder with the library's.
fn ipld(value: &Value) -> Ipld {
    match value {
        Value::Null => Ipld::Null,
        Value::Bool(b) => Ipld::Bool(*b),
        Value::Int(i) => Ipld::Integer(i128::from(*i)),
        Value::Str(s) => Ipld::String(s.clone()),
        Value::Bytes(b) => Ipld::Bytes(b.clone()),
        Value::Link(cid) => Ipld::Link(ipld_core::cid::Cid::try_from(&cid.as_bytes()[..]).unwrap()),
        Value::List(list) => Ipld::List(list.iter().map(ipld).collect()),
        Value::Map(map) => Ipld::Map(map.iter().map(|(k, v)| (k.clone(), ipld(v))).collect()),
    }
}

// 256 cases each by default; set PROPTEST_CASES for more.
proptest! {
    #[test]
    fn decode_undoes_encode(value in values()) {
        prop_assert_eq!(Value::decode(&value.encode()), Ok(value));
    }

    #[test]
    fn our_encoder_writes_the_bytes_of_the_library(value in values()) {
        prop_assert_eq!(value.encode(), serde_ipld_dagcbor::to_vec(&ipld(&value)).unwrap());
    }

    #[test]
    fn json_reads_back_to_the_same_bytes(value in values()) {
        // Only a map with a reserved key has no JSON text.
        if let Ok(json) = value.to_json() {
            prop_assert!(!json.contains('\n'));
            prop_assert_eq!(Value::from_json(&json).unwrap().encode(), value.encode());
        } else {
            let shown = format!("{:?}", value);
            prop_assert!(shown.contains("\"$link\"") || shown.contains("\"$bytes\""));
        }
    }

    /// Whatever bytes arrive: no panic, and what is accepted is exactly what we would write.
    #[test]
    fn encode_undoes_decode(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
        if let Ok(value) = Value::decode(&bytes) {
            prop_assert_eq!(value.encode(), bytes);
        }
    }

    /// One byte changed in a canonical encoding: refused, or again canonical.
    #[test]
    fn a_changed_byte_is_refused_or_canonical(value in values(), at in any::<prop::sample::Index>(), to in any::<u8>()) {
        let mut bytes = value.encode();
        let at = at.index(bytes.len());
        bytes[at] = to;
        if let Ok(value) = Value::decode(&bytes) {
            prop_assert_eq!(value.encode(), bytes);
        }
    }
}
