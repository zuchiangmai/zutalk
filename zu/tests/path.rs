//! path.md: paths, their `zu://` URIs, and the syntax of DIDs, types and keys.

mod common;

use common::DID;
use zu::{Error, Path};

#[test]
fn a_path_is_written_as_a_uri() {
    let uri = "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw";
    let path: Path = uri.parse().unwrap();
    assert_eq!(
        (path.by.as_str(), path.ty.as_str(), path.key.as_str()),
        (DID, "ev/event", "kx7q2m5v4tbn3hpw")
    );
    assert_eq!(path, Path::new(DID, "ev/event", "kx7q2m5v4tbn3hpw").unwrap());
    assert_eq!(path.to_string(), uri);
    let web = "zu://did:web:pol.4seas.example/forum/post/3jui7kd54zh2y";
    assert_eq!(web.parse::<Path>().unwrap().to_string(), web);
}

/// path.md § Examples. A `Path` names one record of a DID, so the valid URIs that name less,
/// or that name the individual by a handle, are not paths either.
#[test]
fn uris_that_are_not_the_path_of_a_record() {
    let refused = [
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event",
        "zu://someone.4seas.example/ev/event/kx7q2m5v4tbn3hpw",
        // The invalid URIs of the specification.
        "zu://someone.4seas.example/",
        "zu://user:pass@someone.4seas.example",
        "zu://someone.4seas.example:3000",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/event",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/Ev/event/kx7q2m5v4tbn3hpw",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw?x=1",
        // And more of the same kind.
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw/",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw#x",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz//ev/event/kx7q2m5v4tbn3hpw",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev//kx7q2m5v4tbn3hpw",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/a/b",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/..",
        "zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/%6Bey",
        "zu:///ev/event/kx7q2m5v4tbn3hpw",
        "zu:/did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw",
        "ZU://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw",
        "at://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw",
        "did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw",
        " zu://did:zu:ewvi7nxzyoun6zhxrhs64oiz/ev/event/kx7q2m5v4tbn3hpw",
        "",
    ];
    for uri in refused {
        assert_eq!(uri.parse::<Path>(), Err(Error::Path), "{uri}");
    }
}

/// path.md § Key Syntax and its examples.
#[test]
fn keys() {
    let long = "k".repeat(512);
    for key in [
        "3jui7kd54zh2y",
        "self",
        "example.com",
        "~1.2-3_",
        "dHJ1ZQ",
        "pre:fix",
        "_",
        "...",
        "A",
        &long,
    ] {
        assert!(Path::new(DID, "ev/event", key).is_ok(), "{key}");
    }
    let longer = "k".repeat(513);
    let invalid = [
        "alpha/beta",
        ".",
        "..",
        "#extra",
        "@handle",
        "any space",
        "any+space",
        "number[3]",
        "number(3)",
        "\"quote\"",
        "dHJ1ZQ==",
        "",
        "%41",
        "é",
        "key\n",
        &longer,
    ];
    for key in invalid {
        assert_eq!(Path::new(DID, "ev/event", key), Err(Error::Path), "{key}");
    }
}

/// path.md § Type Syntax: `/^[a-z][a-z0-9-]{0,31}\/[a-z0-9-]{1,32}$/`.
#[test]
fn types() {
    let (class, kind) = ("c".repeat(32), "9".repeat(32));
    for ty in ["ev/event", "forum/post", "a/b", "a/1", "a-b/-", "a1/1a", &format!("{class}/{kind}")]
    {
        assert!(Path::new(DID, ty, "self").is_ok(), "{ty}");
    }
    let invalid = [
        "event",
        "ev/",
        "/event",
        "ev/event/more",
        "Ev/event",
        "ev/Event",
        "1ev/event",
        "-ev/event",
        "ev/*",
        "ev event",
        "ev/e vent",
        "ev.x/event",
        "ev/event.x",
        "ev_x/event",
        "év/event",
        "",
        "/",
        &format!("{class}c/{kind}"),
        &format!("{class}/{kind}9"),
    ];
    for ty in invalid {
        assert_eq!(Path::new(DID, ty, "self"), Err(Error::Path), "{ty}");
    }
}

/// did.md § DID Identifier Syntax and its examples, for the methods Zu accepts.
#[test]
fn dids() {
    let long = format!("did:zu:{}", "a".repeat(2048 - 7));
    let valid = [
        "did:zu:ewvi7nxzyoun6zhxrhs64oiz",
        "did:web:user.example.com",
        "did:web:localhost%3A8080",
        "did:zu:val:two",
        "did:zu::::val",
        "did:zu:-:_:.",
        "did:zu:v",
        &long,
    ];
    for did in valid {
        assert!(Path::new(did, "ev/event", "self").is_ok(), "{did}");
    }
    let longer = format!("{long}a");
    let invalid = [
        // Valid syntax, but a method Zu does not accept.
        "did:plc:ewvi7nxzyoun6zhxrhs64oiz",
        "did:method:val:two",
        "did:m:v",
        "did:method::::val",
        "did:method:-:_:.",
        "did:key:zQ3shZc2QzApp2oymGvQbzP8eKheVshBHbU4ZYjeXqwSKEn6N",
        "did:webvh:example.com",
        "did:zus:val",
        "did:zu",
        "did:web",
        // Invalid syntax, whatever the method.
        "did:METHOD:val",
        "did:m123:val",
        "DID:method:val",
        "did:method:",
        "did:method:val/two",
        "did:method:val?two",
        "did:method:val#two",
        // The same mistakes under a method Zu accepts.
        "did:ZU:val",
        "DID:zu:val",
        "Did:zu:val",
        "did:zu:",
        "did:zu:val/two",
        "did:zu:val?two",
        "did:zu:val#two",
        "did:zu:val:",
        "did:zu:val%",
        "did:zu:va l",
        "did:zu:val\n",
        "did:zu:é",
        "did:zu:val@x",
        " did:zu:val",
        "zu:val",
        "",
        &longer,
    ];
    for did in invalid {
        assert_eq!(Path::new(did, "ev/event", "self"), Err(Error::Path), "{did}");
    }
}
