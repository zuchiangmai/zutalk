//! cryptography.md: Ed25519, the strict verification rules, and the multikey encoding.

mod common;

use common::{hex, secret};
use curve25519_dalek::constants::EIGHT_TORSION;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::{Scalar, clamp_integer};
use ed25519_dalek::{Verifier, VerifyingKey};
use multibase::Base;
use sha2::{Digest, Sha512};
use zu::{Error, PublicKey, SecretKey};

/// RFC 8032 § 7.1, tests 1 to 3 and the test of 1023 bytes left out: seed, public key,
/// message, signature.
const RFC8032: [[&str; 4]; 3] = [
    [
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "",
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155
         5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    ],
    [
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "72",
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da
         085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    ],
    [
        "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
        "af82",
        "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac
         18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
    ],
];

/// The group order `L`, least significant byte first.
const L: &str = "edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010";

#[test]
fn signs_as_rfc_8032_does() {
    for [seed, public, message, signature] in RFC8032 {
        let secret = SecretKey::from_seed(&hex(seed).try_into().unwrap());
        assert_eq!(secret.public().as_bytes()[..], hex(public));
        assert_eq!(secret.sign(&hex(message))[..], hex(signature));
        let public = PublicKey::from_bytes(&hex(public)).unwrap();
        assert_eq!(public.verify(&hex(message), &hex(signature)), Ok(()));
    }
}

#[test]
fn a_signature_is_for_one_message_and_one_key() {
    let message = b"a record header";
    let sig = secret(1).sign(message);
    let key = secret(1).public();
    assert_eq!(key.verify(message, &sig), Ok(()));
    assert_eq!(key.verify(b"another header", &sig), Err(Error::Signature));
    assert_eq!(secret(2).public().verify(message, &sig), Err(Error::Signature));
    assert_eq!(key.verify(message, &sig[..63]), Err(Error::Signature), "63 bytes");
    assert_eq!(key.verify(message, &[&sig[..], &[0]].concat()), Err(Error::Signature), "65 bytes");
    assert_eq!(key.verify(message, &[]), Err(Error::Signature), "no bytes");
    for bit in 0..512 {
        let mut changed = sig;
        changed[bit / 8] ^= 1 << (bit % 8);
        assert_eq!(key.verify(message, &changed), Err(Error::Signature), "bit {bit}");
    }
}

/// § Verifying Signatures: `S` is below the group order `L`. `S + L` satisfies the same
/// equation, so a verifier that does not compare would take both.
#[test]
fn refuses_s_not_below_l() {
    let message = b"malleable";
    let sig = secret(1).sign(message);
    let mut carry = 0u16;
    let mut plus_l = sig;
    for (byte, l) in plus_l[32..].iter_mut().zip(hex(L)) {
        let sum = u16::from(*byte) + u16::from(l) + carry;
        (*byte, carry) = (sum as u8, sum >> 8);
    }
    assert_eq!(carry, 0, "S + L still fits in 32 bytes");
    assert_eq!(secret(1).public().verify(message, &plus_l), Err(Error::Signature));
    // `S = L` exactly is `S = 0` to the equation, and is refused as well.
    let mut s_is_l = sig;
    s_is_l[32..].copy_from_slice(&hex(L));
    assert_eq!(secret(1).public().verify(message, &s_is_l), Err(Error::Signature));
}

/// The 8 points of small order, in their canonical encodings and in the others.
fn small_order() -> Vec<[u8; 32]> {
    let mut points: Vec<[u8; 32]> =
        EIGHT_TORSION.iter().map(|point| point.compress().to_bytes()).collect();
    // y = 1 and y = -1 with the sign bit of x = 0 set; y = p (0) and y = p + 1 (1).
    for (low, high) in
        [(0x01, 0x80), (0xec, 0xff), (0xed, 0x7f), (0xed, 0xff), (0xee, 0x7f), (0xee, 0xff)]
    {
        let mut point = if low == 0x01 { [0; 32] } else { [0xff; 32] };
        (point[0], point[31]) = (low, high);
        points.push(point);
    }
    points
}

#[test]
fn refuses_public_keys_of_small_order() {
    let points = small_order();
    assert_eq!(points.len(), 14);
    for point in points {
        assert_eq!(PublicKey::from_bytes(&point), Err(Error::Key), "{point:02x?}");
    }
}

/// § Verifying Signatures: an encoding whose `y` is not below the field prime is rejected.
/// Such encodings exist for y = 0 .. 18. Those of small order aside, the library decodes
/// them into keys it would verify with; this is the check the kernel adds.
#[test]
fn refuses_public_keys_that_are_not_canonical() {
    let mut taken_by_the_library = 0;
    for y in 0..19u8 {
        for sign in [0x7f, 0xff] {
            let mut point = [0xff; 32];
            (point[0], point[31]) = (0xed + y, sign);
            assert_eq!(PublicKey::from_bytes(&point), Err(Error::Key), "y = p + {y}");
            taken_by_the_library +=
                VerifyingKey::from_bytes(&point).is_ok_and(|key| !key.is_weak()) as u32;
        }
    }
    assert!(taken_by_the_library > 0, "the library alone would not have been enough");
    // The greatest canonical `y`, p - 1, is a point (of order 2); p - 2 or p - 3 may be none.
    let mut below = [0xff; 32];
    (below[0], below[31]) = (0xec, 0x7f);
    assert!(VerifyingKey::from_bytes(&below).is_ok());
}

#[test]
fn refuses_public_keys_of_the_wrong_length_or_off_the_curve() {
    let key = secret(1).public();
    assert_eq!(PublicKey::from_bytes(key.as_bytes()), Ok(key));
    assert_eq!(PublicKey::from_bytes(&key.as_bytes()[..31]), Err(Error::Key));
    assert_eq!(PublicKey::from_bytes(&[&key.as_bytes()[..], &[0]].concat()), Err(Error::Key));
    assert_eq!(PublicKey::from_bytes(&[]), Err(Error::Key));
    // About half of all `y` have no `x` on the curve.
    let off =
        (2..=255u8).map(|y| [y; 32]).filter(|point| PublicKey::from_bytes(point).is_err()).count();
    assert!((60..200).contains(&off), "{off}");
}

/// The secret scalar of a seed (RFC 8032 § 5.1.5), to build signatures by hand.
fn scalar(seed: u8) -> Scalar {
    let hash: [u8; 64] = Sha512::digest([seed; 32]).into();
    Scalar::from_bytes_mod_order(clamp_integer(hash[..32].try_into().unwrap()))
}

/// `k = SHA-512(R || A || M)` as a scalar (RFC 8032 § 5.1.7).
fn challenge(r: &[u8; 32], key: &PublicKey, message: &[u8]) -> Scalar {
    let hash: [u8; 64] = Sha512::new()
        .chain_update(r)
        .chain_update(key.as_bytes())
        .chain_update(message)
        .finalize()
        .into();
    Scalar::from_bytes_mod_order_wide(&hash)
}

/// § Verifying Signatures: `R` is not a point of small order. With `R` the neutral point and
/// `S = k * a`, the equation `[S]B = R + [k]A` holds, and the library's ordinary verification
/// accepts the signature. The strict rules refuse it.
#[test]
fn refuses_r_of_small_order_even_when_the_equation_holds() {
    let (message, key) = (b"small order R", secret(1).public());
    let r = EIGHT_TORSION[0].compress().to_bytes();
    assert_eq!(r, [&[1u8][..], &[0; 31]].concat()[..], "the neutral point");
    let s = challenge(&r, &key, message) * scalar(1);
    let sig = [r, s.to_bytes()].concat();
    let library = VerifyingKey::from_bytes(key.as_bytes()).unwrap();
    assert!(library.verify(message, &ed25519_dalek::Signature::from_slice(&sig).unwrap()).is_ok());
    assert_eq!(key.verify(message, &sig), Err(Error::Signature));
    // Every other encoding of a point of small order as `R`, with the `S` that goes with it.
    for r in small_order() {
        let s = challenge(&r, &key, message) * scalar(1);
        assert_eq!(
            key.verify(message, &[r, s.to_bytes()].concat()),
            Err(Error::Signature),
            "{r:02x?}"
        );
    }
}

/// § Verifying Signatures: the equation holds without the cofactor. Add a point of order 8 to
/// `R`: `[8][S]B = [8]R + [8][k]A` still holds, which is all that ZIP-215 asks for, but
/// `[S]B = R + [k]A` does not.
#[test]
fn refuses_a_signature_that_only_holds_with_the_cofactor() {
    let (message, key) = (b"cofactor", secret(1).public());
    let nonce = Scalar::from(123456789u64);
    for torsion in &EIGHT_TORSION[1..] {
        let r_point = EdwardsPoint::mul_base(&nonce) + torsion;
        let r = r_point.compress().to_bytes();
        let k = challenge(&r, &key, message);
        let s = nonce + k * scalar(1);
        let a_point = CompressedEdwardsY(*key.as_bytes()).decompress().unwrap();
        let with_cofactor = EdwardsPoint::mul_base(&s).mul_by_cofactor()
            == (r_point + a_point * k).mul_by_cofactor();
        assert!(with_cofactor && !r_point.is_small_order());
        assert_eq!(key.verify(message, &[r, s.to_bytes()].concat()), Err(Error::Signature));
    }
    // The same construction without the point of order 8 is an ordinary valid signature.
    let r = EdwardsPoint::mul_base(&nonce).compress().to_bytes();
    let s = nonce + challenge(&r, &key, message) * scalar(1);
    assert_eq!(key.verify(message, &[r, s.to_bytes()].concat()), Ok(()));
}

/// § Verifying Signatures: `R` is a canonical encoding. The neutral point written with
/// y = p + 1 instead of y = 1 is the same point, and the equation would hold for it.
#[test]
fn refuses_r_that_is_not_canonical() {
    let (message, key) = (b"R not canonical", secret(1).public());
    for y in 0..19u8 {
        let mut r = [0xff; 32];
        (r[0], r[31]) = (0xed + y, 0x7f);
        let s = challenge(&r, &key, message) * scalar(1);
        assert_eq!(
            key.verify(message, &[r, s.to_bytes()].concat()),
            Err(Error::Signature),
            "y = p + {y}"
        );
    }
}

/// § Public Key Encoding: the example of the specification.
#[test]
fn multikey_example_of_the_specification() {
    let text = "z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    let key: PublicKey = text.parse().unwrap();
    assert_eq!(key.to_string(), text);
    let bytes = Base::Base58Btc.decode(&text[1..]).unwrap();
    assert_eq!(bytes[..2], [0xed, 0x01]);
    assert_eq!(bytes[2..], key.as_bytes()[..]);
}

#[test]
fn multikey_is_one_string_for_one_key() {
    for seed in 0..=255 {
        let key = secret(seed).public();
        let text = key.to_string();
        assert!(text.starts_with("z6Mk") && text.len() == 48, "{text}");
        assert_eq!(text.parse(), Ok(key));
    }
}

fn multikey(codec: &[u8], key: &[u8]) -> String {
    format!("z{}", Base::Base58Btc.encode([codec, key].concat()))
}

#[test]
fn refuses_what_is_not_an_ed25519_multikey() {
    let key = secret(1).public();
    let good = key.to_string();
    assert_eq!(multikey(&[0xed, 0x01], key.as_bytes()), good);
    let refused = [
        (String::new(), "empty"),
        ("z".to_owned(), "nothing after the prefix"),
        (good[1..].to_owned(), "no multibase prefix"),
        (format!("Z{}", &good[1..]), "another base58 alphabet"),
        (format!("did:key:{good}"), "a did:key"),
        (format!("{good} "), "space after"),
        (format!("z1{}", &good[1..]), "a leading zero digit"),
        (good.replace('6', "0"), "a character outside base58"),
        (format!("{}é", &good[..46]), "not ASCII"),
        (multikey(&[0xe7, 0x01], &[2; 33]), "secp256k1"),
        (multikey(&[0x80, 0x24], &[2; 33]), "P-256"),
        (multikey(&[0xec, 0x01], key.as_bytes()), "x25519"),
        (multikey(&[0xed], key.as_bytes()), "codec cut short"),
        (multikey(&[0xed, 0x01], &key.as_bytes()[..31]), "31 bytes"),
        (multikey(&[0xed, 0x01], &[&key.as_bytes()[..], &[0]].concat()), "33 bytes"),
        (multikey(&[0xed, 0x01], &[]), "no key"),
        (multikey(&[0xed, 0x01], &small_order()[0]), "a key of small order"),
        (
            multikey(&[0xed, 0x01], &[&[0xf0u8][..], &[0xff; 30], &[0x7f]].concat()),
            "a key that is not canonical",
        ),
        (
            "zQ3shZc2QzApp2oymGvQbzP8eKheVshBHbU4ZYjeXqwSKEn6N".to_owned(),
            "the secp256k1 example of did.md",
        ),
        ("z".repeat(1 << 20), "a megabyte of base58"),
    ];
    for (text, why) in refused {
        assert_eq!(text.parse::<PublicKey>(), Err(Error::Key), "{why}: {text:.60}");
    }
}
