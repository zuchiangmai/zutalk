//! record.md: the signed header of a record, and which record a path refers to.

use crate::did::is_did;
use crate::key::{Map, sealed, take, unseal};
use crate::{Cid, Codec, Error, Path, PublicKey, SecretKey, Value, rule};
use std::{cmp::Reverse, collections::BTreeMap, collections::BTreeSet};

/// § Header. Read or signed here, it has every required field, each of the right type and
/// syntax. Whether it is the individual's is for [`Record::verify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub path: Path,
    /// § Cities: the DID of the city the record is meant for, if it is meant for one.
    pub to: Option<String>,
    pub at: i64,
    /// § Earlier Records, in ascending order of their bytes, without duplicates.
    pub prev: Vec<Cid>,
    /// The CID of the body. `None` removes the record at this path.
    pub body: Option<Cid>,
    pub pubkey: PublicKey,
    pub sig: [u8; 64],
    /// Fields this software does not know. They are kept, because they are signed.
    pub rest: BTreeMap<String, Value>,
}

impl Record {
    /// § Signing. `prev` is sorted and freed of duplicates here. `at` follows § Time, which
    /// only the caller can see to: its clock, and the records it has signed and seen.
    pub fn sign(
        key: &SecretKey,
        path: Path,
        body: Option<Cid>,
        prev: impl IntoIterator<Item = Cid>,
        at: i64,
        to: Option<&str>,
    ) -> Result<Record, Error> {
        let (to, prev) = (to.map(str::to_owned), BTreeSet::from_iter(prev).into_iter().collect());
        let (pubkey, rest) = (key.public(), Map::new());
        let unsigned = Record { path, to, at, prev, body, pubkey, sig: [0; 64], rest };
        let sig = key.sign(&unsigned.value(None).encode());
        Record::try_from(unsigned.value(Some(&sig)))
    }

    /// § Verifying a Record, beyond the header itself: the signature is valid under `pub`;
    /// `key_at(by, pub, at)` says that `pub` is a Zu signing key of `by` for a record signed
    /// at `at`; `at` is greater than `at_of(cid)`, for each record in `prev` the caller holds.
    pub fn verify(
        &self,
        key_at: impl FnOnce(&str, &PublicKey, i64) -> bool,
        at_of: impl Fn(&Cid) -> Option<i64>,
    ) -> Result<(), Error> {
        self.pubkey.verify(&self.value(None).encode(), &self.sig)?;
        rule(key_at(&self.path.by, &self.pubkey, self.at), Error::Signer)?;
        rule(self.prev.iter().filter_map(at_of).all(|at| at < self.at), Error::Time)
    }

    /// § Header: the CID of the record is the CID of its header, `sig` included.
    pub fn cid(&self) -> Cid {
        Cid::of(Codec::Drisl, &self.header().encode())
    }

    /// The header as a map of the data model, as it was signed.
    pub fn header(&self) -> Value {
        self.value(Some(&self.sig))
    }

    fn value(&self, sig: Option<&[u8; 64]>) -> Value {
        let mut map = self.rest.clone();
        map.insert("by".into(), Value::Str(self.path.by.clone()));
        map.insert("type".into(), Value::Str(self.path.ty.clone()));
        map.insert("key".into(), Value::Str(self.path.key.clone()));
        map.insert("at".into(), Value::Int(self.at));
        map.insert(
            "prev".into(),
            Value::List(self.prev.iter().copied().map(Value::Link).collect()),
        );
        map.insert("body".into(), self.body.map_or(Value::Null, Value::Link));
        map.extend(self.to.iter().map(|to| ("to".into(), Value::Str(to.clone()))));
        sealed("record", &self.pubkey, sig, map)
    }
}

/// § Verifying a Record: the checks on the header itself.
impl TryFrom<Value> for Record {
    type Error = Error;
    fn try_from(header: Value) -> Result<Record, Error> {
        let (mut map, pubkey, sig) = unseal(header, "record")?;
        let by = take!(map, "by", Value::Str(by) => by);
        let ty = take!(map, "type", Value::Str(ty) => ty);
        let path = Path::new(&by, &ty, &take!(map, "key", Value::Str(key) => key))?;
        let to = match map.remove("to") {
            Some(Value::Str(to)) if is_did(&to) => Some(to),
            Some(_) => return Err(Error::Field("to")),
            None => None,
        };
        let at = take!(map, "at", Value::Int(at) => at);
        let mut prev = Vec::new();
        for link in take!(map, "prev", Value::List(list) => list) {
            match link {
                Value::Link(cid) if prev.last().is_none_or(|last| *last < cid) => prev.push(cid),
                _ => return Err(Error::Field("prev")),
            }
        }
        let body = take!(map, "body", Value::Null => None, Value::Link(cid) => Some(cid));
        Ok(Record { path, to, at, prev, body, pubkey, sig, rest: map })
    }
}

/// § Which Record a Path Refers To, for every path among `held` at once: each path with its
/// current records, the one it refers to first. `held` are valid records.
pub fn paths(held: &[Record]) -> BTreeMap<&Path, Vec<&Record>> {
    let by_cid: BTreeMap<Cid, &Record> = held.iter().map(|record| (record.cid(), record)).collect();
    let mut by_path: BTreeMap<&Path, Vec<(Cid, &Record)>> = BTreeMap::new();
    let mut cross_path = false;
    for (&cid, &record) in &by_cid {
        by_path.entry(&record.path).or_default().push((cid, record));
        if !cross_path
            && record
                .prev
                .iter()
                .any(|p| by_cid.get(p).is_some_and(|earlier| earlier.path != record.path))
        {
            cross_path = true;
        }
    }
    // Replaced: followed by a record of its own path, directly or through other records.
    let mut replaced = BTreeSet::new();
    if !cross_path {
        for record in by_cid.values() {
            for cid in &record.prev {
                if by_cid.contains_key(cid) {
                    replaced.insert(*cid);
                }
            }
        }
    } else {
        for (&target_path, records) in &by_path {
            let mut seen = BTreeSet::new();
            let mut walk: Vec<Cid> =
                records.iter().flat_map(|(_, r)| r.prev.iter().copied()).collect();
            while let Some(cid) = walk.pop() {
                if let Some(earlier) = by_cid.get(&cid).filter(|_| seen.insert(cid)) {
                    if &earlier.path == target_path {
                        replaced.insert(cid);
                    }
                    walk.extend(&earlier.prev);
                }
            }
        }
    }
    let mut first = BTreeMap::new();
    for (path, records) in by_path {
        let mut current: Vec<(Cid, &Record)> =
            records.into_iter().filter(|(cid, _)| !replaced.contains(cid)).collect();
        if current.is_empty() {
            continue;
        }
        // Concurrent records: the greatest `at` first, then the greatest CID, byte by byte.
        current.sort_by_key(|(cid, record)| Reverse((record.at, *cid)));
        first.insert(path, current.into_iter().map(|(_, record)| record).collect());
    }
    first
}

/// § Which Record a Path Refers To: the current records of `path`, the one it refers to
/// first. `held` are valid records of any path; all of them tell which record follows which.
pub fn current<'a>(held: &'a [Record], path: &Path) -> Vec<&'a Record> {
    paths(held).remove(path).unwrap_or_default()
}

/// § Which Record a Path Refers To: that record, or none if there is none or it was removed.
pub fn resolve<'a>(held: &'a [Record], path: &Path) -> Option<&'a Record> {
    current(held, path).first().copied().filter(|record| record.body.is_some())
}
