//! data-model.md § Link and CID Formats.

mod common;

use common::hex;
use zu::{Cid, Codec, Error};

/// SHA-256 of the empty input (FIPS 180-4), and of `abc`.
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[test]
fn a_cid_is_version_codec_sha256_and_the_hash_of_the_whole_content() {
    let node = Cid::of(Codec::Drisl, b"");
    assert_eq!(node.as_bytes()[..], hex(&format!("01 71 12 20 {EMPTY}")));
    let blob = Cid::of(Codec::Raw, b"abc");
    assert_eq!(blob.as_bytes()[..], hex(&format!("01 55 12 20 {ABC}")));
    assert_ne!(Cid::of(Codec::Raw, b""), node, "the codec is part of the CID");
}

/// The string form: `b` and base32 in lowercase. These are the well-known CIDs of the empty
/// block, as raw bytes and as dag-cbor.
#[test]
fn string_form() {
    let raw = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
    assert_eq!(Cid::of(Codec::Raw, b"").to_string(), raw);
    assert_eq!(raw.parse(), Ok(Cid::of(Codec::Raw, b"")));
    let node = Cid::of(Codec::Drisl, b"");
    assert!(node.to_string().starts_with("bafyrei"), "{node}");
    assert_eq!(node.to_string().len(), 59);
    assert_eq!(node.to_string().parse(), Ok(node));
    assert_eq!(Cid::from_bytes(node.as_bytes()), Ok(node));
}

/// data-model.md § `link`: the example of the specification.
#[test]
fn the_example_of_the_specification() {
    let text = "bafyreidfayvfuwqa7qlnopdjiqrxzs6blmoeu4rujcjtnci5beludirz2a";
    let cid: Cid = text.parse().unwrap();
    assert_eq!(cid.as_bytes()[..4], [0x01, 0x71, 0x12, 0x20]);
    assert_eq!(cid.to_string(), text);
}

#[test]
fn cids_order_by_their_bytes() {
    let mut cids: Vec<Cid> = (0..50u8)
        .map(|n| Cid::of(if n % 2 == 0 { Codec::Raw } else { Codec::Drisl }, &[n]))
        .collect();
    cids.sort();
    assert!(cids.windows(2).all(|pair| pair[0].as_bytes() < pair[1].as_bytes()));
    assert!(
        cids[..25].iter().all(|cid| cid.as_bytes()[1] == 0x55),
        "raw (0x55) sorts before DRISL (0x71)"
    );
}

#[test]
fn refuses_every_other_binary_form() {
    let refused = [
        (format!("01 70 12 20 {EMPTY}"), "codec dag-pb"),
        (format!("01 0129 12 20 {EMPTY}"), "codec dag-json"),
        (format!("01 f100 12 20 {EMPTY}"), "codec not in its shortest form"),
        (format!("00 71 12 20 {EMPTY}"), "version 0"),
        (format!("02 71 12 20 {EMPTY}"), "version 2"),
        (format!("12 20 {EMPTY}"), "CIDv0"),
        (format!("01 71 13 20 {EMPTY}"), "sha-512 by name"),
        (format!("01 71 13 40 {EMPTY}{EMPTY}"), "sha-512"),
        (format!("01 71 1e 20 {EMPTY}"), "blake3"),
        (format!("01 71 12 1f {}", &EMPTY[2..]), "31 bytes of hash"),
        (format!("01 71 12 20 {}", &EMPTY[2..]), "a byte short"),
        (format!("01 71 12 20 {EMPTY} 00"), "a byte long"),
        (format!("00 01 71 12 20 {EMPTY}"), "with the zero byte of a DRISL link"),
        (String::new(), "empty"),
    ];
    for (bytes, why) in refused {
        assert_eq!(Cid::from_bytes(&hex(&bytes)), Err(Error::Cid), "{why}");
    }
}

#[test]
fn refuses_every_other_string_form() {
    let good = Cid::of(Codec::Drisl, b"").to_string();
    let refused = [
        (String::new(), "empty"),
        ("b".to_owned(), "nothing after the prefix"),
        (good[1..].to_owned(), "no multibase prefix"),
        (good.to_uppercase(), "uppercase"),
        (format!("B{}", good[1..].to_uppercase()), "base32 in uppercase"),
        (format!("{good}="), "padding"),
        (format!("{good}a"), "a character too many"),
        (good[..58].to_owned(), "a character short"),
        (format!(" {good}"), "space before"),
        (format!("{good}\n"), "newline after"),
        // The last character carries 3 bits of the hash and 2 bits of nothing, which must be 0.
        (format!("{}{}", &good[..58], "b"), "trailing bits set"),
        ("QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG".to_owned(), "CIDv0"),
        ("zdpuAyvkgEDQm9TenwGkd5eNaosSxjgEYd8QatfPetgB1CdEZ".to_owned(), "base58btc"),
        (
            "f01711220e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
            "base16",
        ),
        ("bafybeihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku".to_owned(), "codec dag-pb"),
        ("bé".to_owned(), "not ASCII"),
        ("z".repeat(1 << 20), "a megabyte of base58"),
    ];
    for (text, why) in refused {
        assert_eq!(text.parse::<Cid>(), Err(Error::Cid), "{why}");
    }
}
