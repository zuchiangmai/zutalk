//! city.md § Working Out the State, for grants: whether a grant's signer is a coordinator in
//! office at the grant's `at`, worked out here from the city's governance chain. The chain is
//! asked of the city's host (the `#pol` entry of the city's DID document), every record of it is
//! verified, and the rules are `zu::city`'s. Nothing is taken on the host's word but which
//! records it holds (decision 0189).

use crate::{BoxFuture, Directory, Refusal, Server, lock};
use axum::http::StatusCode;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use zu::city::{self, Governance, INVITE_TYPE, Invitation, ROLE_TYPE, Role, Term};
use zu::{Cid, Doc, Held, Query, Record};
use zutalk_log::{info, warn};

/// A city's host. Asking it is a port: the server only reads what comes back.
pub trait Hosts: Send + Sync {
    /// The body of a 200 answer to `GET url`; `None` when there is none: unreachable, too slow,
    /// too large, or another status.
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Option<Vec<u8>>>;
}

/// The port of a server that serves no city: there is no host to ask.
pub struct NoHosts;

impl Hosts for NoHosts {
    fn get<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { None })
    }
}

/// How long a chain is used before it is asked for again, in ms. Within it a coordinator
/// dismissed after the fetch can still grant; five minutes is short beside a grant's life.
pub const CITY_TTL_MS: i64 = 5 * 60 * 1000;
/// How soon a chain is asked for again when a grant's signer is not in office in the one held,
/// in ms: a new coordinator is seen this soon, and a stream of refused grants (pol's relay
/// retries every minute) costs the host one query a city per this long at most.
pub const CITY_REFETCH_MS: i64 = 10_000;
/// The most cities whose chains are held. Only the cities in `ZUTALK_CITIES` are ever asked
/// for, so this is a guard, not a working limit.
pub const CITY_CACHE_MAX: usize = 64;
/// The most governance records and invitations read for one city. A city of thousands of
/// residents has a few records each; beyond this the host's answer is not used.
pub const CITY_MAX_RECORDS: usize = 50_000;
/// The most pages of one query, so a host that answers `next` forever is cut off.
pub const CITY_MAX_PAGES: usize = 1_000;
/// How long one fetch of a chain may take in all, in ms. Each signer's document is resolved one
/// after another, once per record never verified before; a cold start of a city of a hundred
/// residents is a hundred lookups.
pub const CITY_FETCH_BUDGET_MS: u64 = 60_000;
/// How long a grant waits for a fetch, in ms. pol's relay gives up after 15 s and asks again a
/// minute later; the fetch goes on without it and the next attempt finds the chain held.
pub const CITY_WAIT_MS: u64 = 12_000;

/// The grant is not addressed (`to`) to a city this server serves.
pub const NOT_SERVED: Refusal = Refusal(StatusCode::FORBIDDEN, "city");
/// The signer is neither listed nor a coordinator in office at the grant's `at`.
pub const NOT_COORDINATOR: Refusal = Refusal(StatusCode::FORBIDDEN, "coordinator");
/// The city's DID document names no `#pol` service of type `city` with an `https` origin.
pub const CITY_DOC: Refusal = Refusal(StatusCode::BAD_GATEWAY, "city-doc");
/// The city's host gave no usable answer.
pub const CITY_HOST: Refusal = Refusal(StatusCode::BAD_GATEWAY, "city-host");
/// The chain did not come within `CITY_WAIT_MS` or `CITY_FETCH_BUDGET_MS`.
pub const CITY_SLOW: Refusal = Refusal(StatusCode::GATEWAY_TIMEOUT, "city-host");

/// How long a chain is kept, and how soon it is asked for again on a miss; `Config::city`.
#[derive(Clone, Copy, Debug)]
pub struct CityTimes {
    pub ttl_ms: i64,
    pub refetch_ms: i64,
}

impl Default for CityTimes {
    fn default() -> CityTimes {
        CityTimes { ttl_ms: CITY_TTL_MS, refetch_ms: CITY_REFETCH_MS }
    }
}

/// A city's chain as it was worked out from one fetch.
#[derive(Clone, Debug)]
struct Known {
    terms: Arc<Vec<Term>>,
    /// When the fetch started, in ms.
    fetched_ms: i64,
    /// The records that passed verification. A record stays valid (record.md § Keys and the DID
    /// Document), so the next fetch verifies only those not here.
    verified: Arc<HashSet<Cid>>,
}

type Fetched = Option<Result<Known, Refusal>>;

#[derive(Default)]
struct Entry {
    known: Option<Known>,
    /// When a fetch last started, in ms.
    asked_ms: i64,
    /// Why the last fetch failed, until one succeeds.
    failed: Option<Refusal>,
    /// The fetch under way: every grant that needs it waits on this one.
    pending: Option<watch::Receiver<Fetched>>,
}

/// The chains of the cities served, by city DID.
#[derive(Default)]
pub(crate) struct Cities(Mutex<HashMap<String, Entry>>);

/// Which path admitted a grant, as its event says it.
pub(crate) const VIA_LIST: &str = "list";
pub(crate) const VIA_CITY: &str = "city";

/// Whether `record`, a verified grant, counts: its signer is on the transition list
/// (`ZUTALK_COORDINATORS`), or it is addressed to a city in `ZUTALK_CITIES` and its signer is a
/// coordinator in office there at its `at`. The answer is the path that admitted it.
pub(crate) async fn admit(
    server: &Arc<Server>,
    record: &Record,
    now: i64,
) -> Result<&'static str, Refusal> {
    let (by, cities) = (&record.path.by, &server.config.cities);
    if server.config.coordinators.contains(by) {
        return Ok(VIA_LIST);
    }
    let Some(city) = record.to.as_ref().filter(|to| cities.contains(to)) else {
        // With no city served, the list was the only question, as before.
        return Err(if cities.is_empty() { NOT_COORDINATOR } else { NOT_SERVED });
    };
    match in_office(server, city, by, record.at, now).await? {
        true => Ok(VIA_CITY),
        false => Err(NOT_COORDINATOR),
    }
}

/// What to do for one question about a city: answer from the chain held, wait for the fetch
/// under way, or start one.
enum Next {
    Answer(Result<bool, Refusal>),
    Wait(watch::Receiver<Fetched>),
    Fetch(Arc<HashSet<Cid>>),
}

/// The rule of the cache, a function of values: a fresh chain that has `did` in office answers
/// at once; a miss asks again unless the last fetch was less than `refetch_ms` ago; a fetch
/// under way is waited for rather than doubled; a fetch that failed that recently is not tried
/// again sooner.
fn next(entry: &Entry, times: CityTimes, now: i64, did: &str, at: i64) -> Next {
    let fresh = entry.known.as_ref().filter(|known| now - known.fetched_ms < times.ttl_ms);
    if fresh.is_some_and(|known| city::held_at(&known.terms, did, Role::Coordinator, at)) {
        return Next::Answer(Ok(true));
    }
    if let Some(pending) = &entry.pending {
        return Next::Wait(pending.clone());
    }
    let recent = now - entry.asked_ms < times.refetch_ms;
    match (fresh, recent, entry.failed) {
        (Some(_), true, _) => Next::Answer(Ok(false)),
        (None, true, Some(failed)) => Next::Answer(Err(failed)),
        _ => Next::Fetch(entry.known.as_ref().map(|k| k.verified.clone()).unwrap_or_default()),
    }
}

/// Whether `did` is a coordinator of `city` in office at `at`, by the chain held or fetched now.
async fn in_office(
    server: &Arc<Server>,
    city: &str,
    did: &str,
    at: i64,
    now: i64,
) -> Result<bool, Refusal> {
    let mut pending = {
        let mut entries = lock(&server.cities.0);
        if !entries.contains_key(city) && entries.len() >= CITY_CACHE_MAX {
            let idle = entries.iter().filter(|(_, e)| e.pending.is_none());
            if let Some(oldest) = idle.min_by_key(|(_, e)| e.asked_ms).map(|(c, _)| c.clone()) {
                entries.remove(&oldest);
            }
        }
        let entry = entries.entry(city.to_owned()).or_default();
        match next(entry, server.config.city, now, did, at) {
            Next::Answer(answer) => return answer,
            Next::Wait(pending) => pending,
            Next::Fetch(verified) => {
                let (sender, pending) = watch::channel(None);
                (entry.pending, entry.asked_ms) = (Some(pending.clone()), now);
                start(server, city, verified, sender, now);
                pending
            }
        }
    };
    let wait = Duration::from_millis(CITY_WAIT_MS);
    let fetched = match tokio::time::timeout(wait, pending.wait_for(Option::is_some)).await {
        Err(_) => return Err(CITY_SLOW),
        Ok(Err(_)) => return Err(CITY_HOST), // the fetch ended without saying: it was cancelled
        Ok(Ok(fetched)) => fetched.clone(),
    };
    let known = fetched.ok_or(CITY_HOST)??;
    Ok(city::held_at(&known.terms, did, Role::Coordinator, at))
}

/// Fetches `city`'s chain on a supervised task of its own, so that it is held even when the
/// grant that asked for it has gone, and says what came of it to whoever waits.
fn start(
    server: &Arc<Server>,
    city: &str,
    verified: Arc<HashSet<Cid>>,
    sender: watch::Sender<Fetched>,
    now: i64,
) {
    let (owner, city) = (server.clone(), city.to_owned());
    // Not awaited: it answers through `sender`, and the tracker waits for it at shutdown.
    server.tasks.spawn("city.fetch", async move {
        let server = owner;
        let started = Instant::now();
        let budget = Duration::from_millis(CITY_FETCH_BUDGET_MS);
        let work = fetch(&*server.directory, &*server.hosts, &city, &verified, now);
        let fetched = tokio::time::timeout(budget, work).await.unwrap_or(Err(CITY_SLOW));
        let ms = started.elapsed().as_millis() as u64;
        let fetched = fetched.map(|(known, said)| {
            let Said { records, invitations, discarded, verified, forks } = said;
            if forks > 0 {
                warn!("city.fork", forks = forks);
            }
            let terms = known.terms.len();
            info!(
                "city.fetch",
                code = "ok",
                records = records,
                invitations = invitations,
                discarded = discarded,
                verified = verified,
                terms = terms,
                ms = ms
            );
            known
        });
        if let Err(refusal) = &fetched {
            warn!("city.fetch", code = refusal.1, ms = ms);
        }
        if let Some(entry) = lock(&server.cities.0).get_mut(&city) {
            entry.pending = None;
            match &fetched {
                Ok(known) => (entry.known, entry.failed) = (Some(known.clone()), None),
                Err(refusal) => entry.failed = Some(*refusal),
            }
        }
        sender.send_replace(Some(fetched));
    });
}

/// What one fetch saw, for its event.
struct Said {
    records: usize,
    invitations: usize,
    /// Records that failed verification, and were left out.
    discarded: usize,
    /// Records verified in this fetch, not already known valid.
    verified: usize,
    forks: usize,
}

/// § Marking a City and a Host: the origin of the city's host, from its `#pol` entry.
pub fn host_of(doc: &Doc) -> Option<String> {
    let (kind, endpoint) = doc.services.get("pol")?;
    let host = endpoint.strip_prefix("https://")?.trim_end_matches('/');
    let host = zu::did::handle(host).filter(|_| kind == "city")?;
    Some(format!("https://{host}"))
}

/// § Working Out the State: every governance record and invitation of `city`, verified, ordered
/// into the chain and worked out into office terms.
async fn fetch(
    directory: &dyn Directory,
    hosts: &dyn Hosts,
    city: &str,
    verified: &HashSet<Cid>,
    now: i64,
) -> Result<(Known, Said), Refusal> {
    let doc = directory.doc(city).await.map_err(|refusal| match refusal.1 {
        "directory" => refusal,
        _ => CITY_DOC,
    })?;
    let origin = host_of(&doc).ok_or(CITY_DOC)?;
    let roles = all(hosts, &origin, city, ROLE_TYPE).await?;
    let invites = all(hosts, &origin, city, INVITE_TYPE).await?;
    if roles.records.len() + invites.records.len() > CITY_MAX_RECORDS {
        return Err(CITY_HOST);
    }

    // record.md § Verifying a Record, for each record never verified before; one document a
    // signer. A directory that cannot be reached fails the fetch: a chain without some of its
    // records could leave a dismissal out.
    let held: Vec<&Record> = roles.records.iter().chain(&invites.records).collect();
    let ats: HashMap<Cid, i64> = held.iter().map(|r| (r.cid(), r.at)).collect();
    let mut docs: HashMap<&str, Option<Doc>> = HashMap::new();
    for record in held.iter().filter(|r| !verified.contains(&r.cid())) {
        let by = record.path.by.as_str();
        if docs.contains_key(by) {
            continue;
        }
        let doc = match directory.doc(by).await {
            Ok(doc) => Some(doc),
            Err(refusal) if refusal.1 == "directory" => return Err(refusal),
            Err(_) => None, // no DID of Zu's, or no document: none of its records is valid
        };
        docs.insert(by, doc);
    }
    let valid = |record: &Record| {
        let cid = record.cid();
        let doc = docs.get(record.path.by.as_str()).and_then(Option::as_ref);
        let checked = doc.is_some_and(|doc| {
            let key_at = |_: &str, key: &zu::PublicKey, at: i64| doc.key_at(key, at);
            record.verify(key_at, |cid| ats.get(cid).copied()).is_ok()
        });
        (verified.contains(&cid) || checked).then_some(cid)
    };
    let mut now_valid = HashSet::new();
    let mut steps = Vec::new();
    for record in &roles.records {
        if let Some(cid) = valid(record) {
            now_valid.insert(cid);
            let body = record.body.and_then(|body| roles.bodies.get(&body));
            steps.push(Governance::new(record.clone(), body));
        }
    }
    let mut current = Vec::new();
    for record in &invites.records {
        if let Some(cid) = valid(record) {
            now_valid.insert(cid);
            current.push(record.clone());
        }
    }
    // § Invitations: one counts only while it is current at its path.
    let invitations: Vec<Invitation> = zu::record::paths(&current)
        .into_values()
        .filter_map(|records| {
            let record = records.first()?;
            Invitation::new(record, invites.bodies.get(&record.body?)?)
        })
        .collect();

    let chain = city::order(steps);
    let terms = city::terms(city, &chain.chain, &invitations);
    let said = Said {
        records: roles.records.len(),
        invitations: invites.records.len(),
        discarded: held.len() - now_valid.len(),
        verified: now_valid.iter().filter(|cid| !verified.contains(cid)).count(),
        forks: chain.forks.len(),
    };
    let known = Known { terms: Arc::new(terms), fetched_ms: now, verified: Arc::new(now_valid) };
    Ok((known, said))
}

/// query.md: every record of type `ty` meant for `city`, replaced and removed ones too, page
/// by page.
async fn all(hosts: &dyn Hosts, origin: &str, city: &str, ty: &str) -> Result<Held, Refusal> {
    let mut query = Query { all: true, ..Query::across(ty, Some(city)) };
    let mut held = Held::default();
    for _ in 0..CITY_MAX_PAGES {
        let url = query.url(origin).map_err(|_| CITY_DOC)?;
        let answer = hosts.get(&url).await.ok_or(CITY_HOST)?;
        let (page, next) = query.read(&answer).map_err(|_| CITY_HOST)?;
        held.records.extend(page.records);
        held.bodies.extend(page.bodies);
        if held.records.len() > CITY_MAX_RECORDS {
            return Err(CITY_HOST);
        }
        match next {
            None => {
                // Pages a host cut at different places may repeat a record.
                let mut seen = HashSet::new();
                held.records.retain(|record| seen.insert(record.cid()));
                return Ok(held);
            }
            Some(next) if query.from.as_ref() == Some(&next) => return Err(CITY_HOST),
            Some(next) => query.from = Some(next),
        }
    }
    Err(CITY_HOST)
}
