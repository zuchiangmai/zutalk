//! query.md: asking a host for records, and reading what it answers. Giving records to a
//! host (record.md § Delivering) is a request with the fields of an answer.

use crate::did::is_did;
use crate::key::{Map, take};
use crate::{Cid, Codec, Error, Held, Path, Record, Value, rule};
use std::cmp::Reverse;

/// § The Query: a filter over what record headers state, and the parameters that shape the
/// answer. A record matches when it matches every parameter given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub by: Option<String>,
    pub to: Option<String>,
    /// A type, or every type of one class when written with a star, as in `ev/*`.
    pub ty: Option<String>,
    pub key: Option<String>,
    /// The header's `at` is this time or later.
    pub since: Option<i64>,
    /// The header's `at` is earlier than this time.
    pub until: Option<i64>,
    /// The most records to return in this answer. A host may return fewer.
    pub n: Option<u32>,
    /// The `next` of an earlier answer, to continue after it.
    pub from: Option<String>,
    /// Also return records that have been replaced or removed.
    pub all: bool,
}

impl Query {
    /// § The Query, the common filters: the record at one path.
    pub fn one(path: &Path) -> Query {
        Query { key: Some(path.key.clone()), ..Query::of(&path.by, &path.ty) }
    }

    /// An individual's records of one type.
    pub fn of(by: &str, ty: &str) -> Query {
        Query { by: Some(by.into()), ..Query::across(ty, None) }
    }

    /// The records of one type from everyone the host holds records for: all of them, or
    /// those meant for the city `to`.
    pub fn across(ty: &str, to: Option<&str>) -> Query {
        Query { ty: Some(ty.into()), to: to.map(str::to_owned), ..Query::default() }
    }

    /// § The Query: the URL to `GET` at the host of this origin, such as
    /// `https://pol.4seas.example`. With no parameter it is where records are delivered
    /// (`POST`). A DID, a type or a key of the wrong syntax is refused.
    pub fn url(&self, origin: &str) -> Result<String, Error> {
        // Checked as a path: a star stands for a kind, and what is left out for anything.
        let star = |ty: &str| ty.strip_suffix("/*").map_or(ty.into(), |class| format!("{class}/a"));
        let ty = self.ty.as_deref().map_or("a/a".into(), star);
        let by = self.by.as_deref().unwrap_or("did:zu:a");
        Path::new(by, &ty, self.key.as_deref().unwrap_or("a"))?;
        rule(self.to.as_deref().is_none_or(is_did), Error::Path)?;
        // What a query string does not take as it is, is percent-encoded.
        let plain = |b: u8| b.is_ascii_alphanumeric() || b"-._~:/*".contains(&b);
        let escape = |b| if plain(b) { char::from(b).to_string() } else { format!("%{b:02X}") };
        let text = |value: &Option<String>| value.as_ref().map(|v| v.bytes().map(escape).collect());
        let number = |value: Option<i64>| value.map(|value| value.to_string());
        let params: [(&str, Option<String>); 9] = [
            ("by", text(&self.by)),
            ("to", text(&self.to)),
            ("type", text(&self.ty)),
            ("key", text(&self.key)),
            ("since", number(self.since)),
            ("until", number(self.until)),
            ("n", number(self.n.map(i64::from))),
            ("from", text(&self.from)),
            ("all", self.all.then(|| "1".into())),
        ];
        let given = params.iter().filter_map(|(name, v)| Some(format!("{name}={}", v.as_ref()?)));
        let query = given.collect::<Vec<_>>().join("&");
        let mark = if query.is_empty() { "" } else { "?" };
        Ok(format!("{origin}/.well-known/zu-recs{mark}{query}"))
    }

    /// § The Query: whether a record matches the filter.
    pub fn matches(&self, record: &Record) -> bool {
        let is = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
        let ty = &record.path.ty;
        let star = |class: &str| class.ends_with('/') && ty.starts_with(class);
        is(&self.by, &record.path.by)
            && self.to.as_ref().is_none_or(|to| record.to.as_ref() == Some(to))
            && self.ty.as_deref().is_none_or(|t| t == ty || t.strip_suffix('*').is_some_and(star))
            && is(&self.key, &record.path.key)
            && self.since.is_none_or(|since| record.at >= since)
            && self.until.is_none_or(|until| record.at < until)
    }

    /// § The Answer: read the body of a 200 answer to this query, as what it holds, the latest
    /// record first, and its `next`, which goes back to the host as `from`. A header that is
    /// not a header (record.md § Verifying a Record, the checks on the header itself) or does
    /// not match the filter is discarded, and so is a body that no header kept links to by its
    /// CID. Whether a record is the individual's is for [`Record::verify`], before any use.
    pub fn read(&self, answer: &[u8]) -> Result<(Held, Option<String>), Error> {
        let Value::Map(mut map) = Value::decode(answer)? else { return Err(Error::Field("recs")) };
        let headers = take!(map, "recs", Value::List(recs) => recs).into_iter();
        map.entry("blocks".into()).or_insert_with(|| Value::Map(Map::new()));
        let blocks = take!(map, "blocks", Value::Map(blocks) => blocks);
        let next = map.remove("next").map_or(Ok(None), |next| match next {
            Value::Str(next) => Ok(Some(next)),
            _ => Err(Error::Field("next")),
        })?;
        let records = headers.filter_map(|header| Record::try_from(header).ok());
        let mut records: Vec<Record> = records.filter(|record| self.matches(record)).collect();
        records.sort_by_cached_key(|record| Reverse((record.at, record.cid())));
        records.dedup();
        let bodies = blocks.into_iter().filter_map(|(name, body)| {
            let cid = Cid::of(Codec::Drisl, &body.encode());
            let linked = records.iter().any(|record| record.body == Some(cid));
            (linked && name.parse() == Ok(cid) && matches!(body, Value::Map(_)))
                .then_some((cid, body))
        });
        Ok((Held { bodies: bodies.collect(), records }, next))
    }
}

/// § The Answer, as a host writes it: the headers in their order, the bodies it chose to send
/// along, and `next` when more records match. record.md § Delivering: the body of the request
/// that gives records to a host.
pub fn answer(held: &Held, next: Option<&str>) -> Vec<u8> {
    let mut records: Vec<&Record> = held.records.iter().collect();
    records.sort_by_cached_key(|record| Reverse((record.at, record.cid())));
    let recs = Value::List(records.iter().map(|record| record.header()).collect());
    let blocks = held.bodies.iter().map(|(cid, body)| (cid.to_string(), body.clone()));
    let next = next.map(|next| ("next".to_owned(), Value::Str(next.into())));
    let fields = [("recs".to_owned(), recs), ("blocks".to_owned(), Value::Map(blocks.collect()))];
    Value::Map(fields.into_iter().chain(next).collect()).encode()
}
