//! path.md: the address of a record, individual > type > key, and its `zu://` URI.

use crate::{Error, did::is_did, rule};
use std::{fmt, str::FromStr};

/// The path of one record: the DID of the individual, the type such as `ev/event`, the key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path {
    pub by: String,
    pub ty: String,
    pub key: String,
}

/// 1 to `max` characters: ASCII digits, lowercase letters, `more`, and uppercase if `upper`.
fn word(text: &str, max: usize, upper: bool, more: &[u8]) -> bool {
    let letter = |b: u8| b.is_ascii_alphanumeric() && (upper || !b.is_ascii_uppercase());
    (1..=max).contains(&text.len()) && text.bytes().all(|b| letter(b) || more.contains(&b))
}

impl Path {
    /// `by` is a DID of a method Zu accepts; § Type Syntax; § Key Syntax.
    pub fn new(by: &str, ty: &str, key: &str) -> Result<Path, Error> {
        let (class, kind) = ty.split_once('/').unwrap_or_default();
        let typed = word(class, 32, false, b"-") && word(kind, 32, false, b"-");
        let keyed = word(key, 512, true, b".-_:~") && key != "." && key != "..";
        rule(is_did(by) && typed && class.starts_with(char::is_lowercase) && keyed, Error::Path)?;
        Ok(Path { by: by.into(), ty: ty.into(), key: key.into() })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "zu://{}/{}/{}", self.by, self.ty, self.key)
    }
}

/// § `zu://` URI Syntax, for a URI that names one record of a DID, in normalized form.
impl FromStr for Path {
    type Err = Error;
    fn from_str(uri: &str) -> Result<Path, Error> {
        let (by, rest) =
            uri.strip_prefix("zu://").unwrap_or("").split_once('/').ok_or(Error::Path)?;
        let (ty, key) = rest.rsplit_once('/').ok_or(Error::Path)?;
        Path::new(by, ty, key)
    }
}
