//! blob.md, retrieval.md: blob references, the upload, and content fetched by its CID.

mod common;

use common::{cid, fields, hex, map, text};
use proptest::prelude::*;
use zu::blob::{block, check, rasl, refs, upload};
use zu::{Blob, Cid, Codec, Error, Value};

const HOST: &str = "https://pol.4seas.example";
const FILE: &[u8] = b"the bytes of a file";

fn file() -> Blob {
    Blob::of(FILE, "text/plain").unwrap()
}

/// data-model.md § `blob` Type: `$type`, `ref` with the `raw` codec, `mime`, `size`.
#[test]
fn a_blob_reference_has_exactly_the_fields_of_the_specification() {
    let blob = file();
    assert_eq!(blob, Blob { cid: Cid::of(Codec::Raw, FILE), mime: "text/plain".into(), size: 19 });
    let expected = map([
        ("$type", text("blob")),
        ("ref", Value::Link(Cid::of(Codec::Raw, FILE))),
        ("mime", text("text/plain")),
        ("size", Value::Int(19)),
    ]);
    assert_eq!(blob.value(), expected);
    assert_eq!(refs(&blob.value()), [blob]);
    // A blob is hashed whole, and its CID says that it is raw bytes: `bafkrei…`.
    assert!(file().cid.to_string().starts_with("bafkrei"));
    assert_eq!(file().cid.as_bytes()[1], 0x55);
    // An empty file is a file. A media type is not empty.
    assert_eq!(Blob::of(b"", "application/octet-stream").map(|blob| blob.size), Ok(0));
    assert_eq!(Blob::of(FILE, ""), Err(Error::Field("mime")));
}

/// blob.md § Keeping and Discarding: a host finds the blob references in a body by their
/// shape, at any depth, without knowing the record's type.
#[test]
fn blob_references_are_found_at_any_depth() {
    let (image, sound) =
        (Blob::of(b"image", "image/png").unwrap(), Blob::of(b"sound", "audio/ogg").unwrap());
    let body = map([
        ("name", text("a body")),
        ("cover", image.value()),
        (
            "parts",
            Value::List(vec![
                Value::Int(1),
                map([("deep", map([("deeper", Value::List(vec![sound.value()]))]))]),
                image.value(),
            ]),
        ),
        ("link", Value::Link(cid("another record"))),
    ]);
    assert_eq!(refs(&body), [image.clone(), sound, image.clone()]);
    assert_eq!(refs(&map([("name", text("no file"))])), []);
    assert_eq!(refs(&Value::Null), []);
    assert_eq!(refs(&Value::List(vec![image.value()])), [image]);
}

/// data-model.md § `blob` Type: a map of another shape is no blob reference.
#[test]
fn a_map_of_another_shape_is_no_blob_reference() {
    let good = fields(file().value());
    let changes = [
        ("$type", Some(text("Blob"))),
        ("$type", Some(text("record"))),
        ("$type", None),
        ("ref", Some(Value::Link(Cid::of(Codec::Drisl, FILE)))),
        ("ref", Some(text(&file().cid.to_string()))),
        ("ref", Some(Value::Bytes(file().cid.as_bytes().to_vec()))),
        ("ref", None),
        ("mime", Some(text(""))),
        ("mime", Some(Value::Null)),
        ("mime", None),
        ("size", Some(Value::Int(-1))),
        ("size", Some(text("19"))),
        ("size", None),
    ];
    for (name, to) in changes {
        let mut changed = good.clone();
        match to.clone() {
            Some(to) => changed.insert(name.to_owned(), to),
            None => changed.remove(name),
        };
        assert_eq!(refs(&Value::Map(changed)), [], "{name}: {to:?}");
    }
    // What a reference states is not checked here: a size is the writer's statement.
    let mut stated = good.clone();
    stated.insert("size".into(), Value::Int(i64::MAX));
    assert_eq!(refs(&Value::Map(stated))[0].size, i64::MAX as u64);
    // A field that is not known does not undo the shape, and "any depth" is any depth.
    let mut more = good.clone();
    more.insert("$later".into(), Value::Int(1));
    assert_eq!(refs(&Value::Map(more.clone())), [file()]);
    let inner = Blob::of(b"smaller", "image/png").unwrap();
    more.insert("$later".into(), inner.value());
    assert_eq!(refs(&Value::Map(more)), [file(), inner]);
}

/// blob.md § Uploading: `PUT https://<host>/.well-known/zu-blob/<cid>`, the CID of the bytes
/// with the `raw` codec.
#[test]
fn a_blob_is_uploaded_under_its_cid() {
    let url = upload(HOST, &file().cid).unwrap();
    assert_eq!(url, format!("https://pol.4seas.example/.well-known/zu-blob/{}", file().cid));
    assert_eq!(
        url,
        "https://pol.4seas.example/.well-known/zu-blob/bafkreiaplgnziupxew76d23zi5vtmqltqdkmgyqs2jsewq3ilzj5odhlfy"
    );
    // The body of a record is no blob.
    assert_eq!(upload(HOST, &Cid::of(Codec::Drisl, FILE)), Err(Error::Cid));
}

/// retrieval.md § RASL in Brief: `https://<host>/.well-known/rasl/<cid>`, for blobs and for
/// blocks alike.
#[test]
fn content_is_fetched_by_its_cid() {
    for cid in [file().cid, cid("a body")] {
        assert_eq!(rasl(HOST, &cid), format!("https://pol.4seas.example/.well-known/rasl/{cid}"));
    }
}

/// retrieval.md § Checking Content: the CID of what was received, computed with the codec of
/// the CID asked for, is the CID asked for. blob.md § Uploading: the host makes the same check.
#[test]
fn content_that_is_not_what_its_cid_says_is_refused() {
    assert_eq!(check(&file().cid, FILE), Ok(()));
    let body = map([("name", text("a body"))]).encode();
    let block_cid = Cid::of(Codec::Drisl, &body);
    assert_eq!(check(&block_cid, &body), Ok(()));
    let wrong: [&[u8]; 4] =
        [b"the bytes of a fil", b"the bytes of a file\n", b"The bytes of a file", b""];
    for content in wrong {
        assert_eq!(check(&file().cid, content), Err(Error::Content));
        assert_eq!(check(&block_cid, content), Err(Error::Content));
    }
    // The same bytes are one thing as a blob and another as a block.
    assert_eq!(check(&Cid::of(Codec::Raw, &body), &body), Ok(()));
    assert_ne!(Cid::of(Codec::Raw, &body), block_cid);
}

/// retrieval.md § Checking Content: content behind a DRISL CID is a block, decoded strictly.
#[test]
fn a_block_is_checked_and_then_decoded_strictly() {
    let body = map([("name", text("a body")), ("file", file().value())]);
    let bytes = body.encode();
    assert_eq!(block(&Cid::of(Codec::Drisl, &bytes), &bytes), Ok(body));
    assert_eq!(block(&Cid::of(Codec::Drisl, b"other"), &bytes), Err(Error::Content));
    // Bytes that hash to the CID and are not DRISL: a float, and a map with its keys unsorted.
    for loose in [hex("fb3ff8000000000000"), hex("a2 6162 01 6161 02")] {
        assert_eq!(block(&Cid::of(Codec::Drisl, &loose), &loose), Err(Error::Drisl));
    }
    // A blob is not a block, whatever its bytes are.
    assert_eq!(block(&Cid::of(Codec::Raw, &bytes), &bytes), Err(Error::Cid));
}

proptest! {
    /// Content passes the check of its own CID and of no other, under either codec.
    #[test]
    fn content_passes_the_check_of_its_own_cid_only(
        a in proptest::collection::vec(any::<u8>(), 0..64),
        b in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        for codec in [Codec::Raw, Codec::Drisl] {
            prop_assert_eq!(check(&Cid::of(codec, &a), &a), Ok(()));
            prop_assert_eq!(check(&Cid::of(codec, &a), &b).is_ok(), a == b);
        }
        let blob = Blob::of(&a, "application/octet-stream").unwrap();
        prop_assert_eq!(refs(&map([("file", blob.value())])), vec![blob]);
    }
}
