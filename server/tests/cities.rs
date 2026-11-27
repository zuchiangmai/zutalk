//! Grants by a city's coordinators (decision 0189): the server asks the city's host for its
//! governance chain, verifies every record, and works out from it whether the grant's signer
//! was a coordinator in office at the grant's `at`. The old list still counts beside it.

mod common;

use axum::http::StatusCode;
use common::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use zu::city::ROLE_TYPE;
use zu::{Cid, Codec, Held, Path, Record, Value};
use zutalk_log::{Level, capture};
use zutalk_server::{CityTimes, now};

/// The city served, and its host.
const CITY: u8 = 20;
const HOST: &str = "https://city.example";
/// Another city, with a host of its own.
const OTHER: u8 = 19;
const OTHER_HOST: &str = "https://other.example";
/// In `CITY`: a coordinator; a resident who signs grants all the same; a coordinator dismissed
/// before the grants; the resident the grants are for.
const COORD: u8 = 5;
const RESIDENT: u8 = 6;
const DISMISSED: u8 = 7;
const GRANTEE: u8 = 1;

static KEY: AtomicU64 = AtomicU64::new(1);

fn key(prefix: &str) -> String {
    format!("{prefix}{}", KEY.fetch_add(1, Ordering::SeqCst))
}

/// A city's governance chain as its host holds it: each record lists the one before it.
struct Chain {
    city: String,
    held: Held,
    last: Option<Cid>,
    at: i64,
}

impl Chain {
    fn new(city: u8) -> Chain {
        Chain { city: did(city), held: Held::default(), last: None, at: now() - DAY }
    }

    /// A `city/role` record `{ who, kind, ok }` of `did(by)`, signed with `secret(signer)`.
    fn signed(&mut self, signer: u8, by: u8, who: u8, kind: &str, ok: bool, acts: &[Cid]) -> Cid {
        let body = map(&[
            ("who", Value::Str(did(who))),
            ("kind", Value::Str(kind.into())),
            ("ok", Value::Bool(ok)),
        ]);
        let cid = Cid::of(Codec::Drisl, &body.encode());
        self.at += 1000;
        let path = Path::new(&did(by), ROLE_TYPE, &key("g")).unwrap();
        let prev = self.last.iter().chain(acts).copied();
        let record =
            Record::sign(&secret(signer), path, Some(cid), prev, self.at, Some(&self.city));
        let record = record.unwrap();
        self.last = Some(record.cid());
        self.held.bodies.insert(cid, body);
        self.held.records.push(record);
        self.last.unwrap()
    }

    fn add(&mut self, by: u8, who: u8, kind: &str, ok: bool, acts: &[Cid]) -> Cid {
        self.signed(by, by, who, kind, ok, acts)
    }

    /// `who` joins and the city admits them.
    fn resident(&mut self, who: u8) {
        let join = self.add(who, who, "resident", true, &[]);
        self.add(self.city_n(), who, "resident", true, &[join]);
    }

    fn city_n(&self) -> u8 {
        if self.city == did(CITY) { CITY } else { OTHER }
    }

    fn appoint(&mut self, by: u8, who: u8, ok: bool) -> i64 {
        self.add(by, who, "coordinator", ok, &[]);
        self.at
    }
}

/// `CITY`'s chain: `COORD` a coordinator, `RESIDENT` and `GRANTEE` residents, and `DISMISSED`
/// a coordinator from the first returned time until the second.
fn city_chain() -> (Chain, i64, i64) {
    let mut chain = Chain::new(CITY);
    chain.resident(COORD);
    chain.appoint(CITY, COORD, true);
    chain.resident(RESIDENT);
    chain.resident(DISMISSED);
    let from = chain.appoint(CITY, DISMISSED, true);
    chain.resident(GRANTEE);
    let until = chain.appoint(CITY, DISMISSED, false);
    (chain, from, until)
}

/// A bench serving `cities`, on no list, whose hosts hold the chains given.
fn served(cities: &[u8], times: CityTimes, chains: Vec<(&str, Chain)>) -> Bench {
    let cities: Vec<String> = cities.iter().map(|n| did(*n)).collect();
    let bench = bench_with(|config| {
        (config.cities, config.coordinators, config.city) = (cities, vec![], times);
    });
    let mut hosts = bench.keys.1.lock().unwrap();
    hosts.insert(did(CITY), HOST.into());
    hosts.insert(did(OTHER), OTHER_HOST.into());
    drop(hosts);
    for (origin, chain) in chains {
        publish(&bench, origin, chain);
    }
    bench
}

fn publish(bench: &Bench, origin: &str, chain: Chain) {
    bench.hosts.held.lock().unwrap().insert(origin.into(), chain.held);
}

/// Asks again on every miss, so a test sees each change of a chain at once.
const EAGER: CityTimes = CityTimes { ttl_ms: 5 * 60 * 1000, refetch_ms: 0 };

/// A grant of 250 cents to `GRANTEE` by `did(by)`, addressed to `to`, signed at `at`.
fn grant_to(by: u8, to: Option<u8>, at: i64) -> String {
    let body = map(&[
        ("to", Value::Str(did(GRANTEE))),
        ("cents", Value::Int(250)),
        ("until", Value::Int(now() + DAY)),
    ]);
    let cid = Cid::of(Codec::Drisl, &body.encode());
    let path = Path::new(&did(by), "talk/grant", &key("k")).unwrap();
    let to = to.map(did);
    let record = Record::sign(&secret(by), path, Some(cid), [], at, to.as_deref()).unwrap();
    delivery(&record, Some(&body))
}

async fn deliver(bench: &Bench, by: u8, to: Option<u8>, at: i64) -> (StatusCode, String) {
    code(&bench.app, "/grants", grant_to(by, to, at)).await
}

fn asked(bench: &Bench) -> u64 {
    bench.hosts.asked.load(Ordering::SeqCst)
}

const OK: (StatusCode, &str) = (StatusCode::OK, "");

fn is(answer: (StatusCode, String), want: (StatusCode, &str)) {
    assert_eq!((answer.0, answer.1.as_str()), want);
}

#[tokio::test]
async fn a_coordinator_in_office_at_the_grants_time_is_admitted() {
    let (chain, ..) = city_chain();
    let bench = served(&[CITY], EAGER, vec![(HOST, chain)]);
    let seen = capture(Level::Debug);
    is(deliver(&bench, COORD, Some(CITY), now()).await, OK);
    assert_eq!(left(&bench, GRANTEE), 2_500_000);
    let admitted = seen.of("grant.admit");
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].text("via"), Some("city"));
    let fetched = seen.of("city.fetch");
    assert_eq!(fetched[0].text("code"), Some("ok"));
    assert_eq!(fetched[0].u64("records"), Some(11));
    assert_eq!(fetched[0].u64("discarded"), Some(0));
    // The chain has more records than a page: `next` was followed.
    assert!(asked(&bench) > 2);
    assert_eq!(seen.refused(), 0);
    // Held now: the next grant asks the host nothing.
    let before = asked(&bench);
    is(deliver(&bench, COORD, Some(CITY), now()).await, OK);
    assert_eq!(asked(&bench), before);
    assert_eq!(left(&bench, GRANTEE), 5_000_000);
}

#[tokio::test]
async fn a_resident_is_no_coordinator() {
    let (chain, ..) = city_chain();
    let bench = served(&[CITY], EAGER, vec![(HOST, chain)]);
    let seen = capture(Level::Debug);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
    is(deliver(&bench, GRANTEE, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
    assert_eq!(left(&bench, GRANTEE), 0);
    let refused = seen.of("grant.refuse");
    assert_eq!(refused.len(), 2);
    assert_eq!(refused[0].text("code"), Some("coordinator"));
}

#[tokio::test]
async fn a_coordinator_counts_only_while_in_office() {
    let (chain, from, until) = city_chain();
    let bench = served(&[CITY], EAGER, vec![(HOST, chain)]);
    let refused = (StatusCode::FORBIDDEN, "coordinator");
    is(deliver(&bench, DISMISSED, Some(CITY), now()).await, refused);
    is(deliver(&bench, DISMISSED, Some(CITY), until).await, refused);
    is(deliver(&bench, DISMISSED, Some(CITY), from - 1).await, refused);
    // Signed while in office, it counts: the grant is judged at its own `at`.
    is(deliver(&bench, DISMISSED, Some(CITY), until - 1).await, OK);
    is(deliver(&bench, DISMISSED, Some(CITY), from).await, OK);
    assert_eq!(left(&bench, GRANTEE), 5_000_000);
}

#[tokio::test]
async fn a_grant_for_a_city_not_served_is_refused() {
    let (chain, ..) = city_chain();
    let mut other = Chain::new(OTHER);
    other.resident(COORD);
    other.appoint(OTHER, COORD, true);
    let bench = served(&[CITY], EAGER, vec![(HOST, chain), (OTHER_HOST, other)]);
    let refused = (StatusCode::FORBIDDEN, "city");
    is(deliver(&bench, COORD, Some(OTHER), now()).await, refused);
    is(deliver(&bench, COORD, None, now()).await, refused);
    assert_eq!(asked(&bench), 0, "no host is asked about a city not served");
    assert_eq!(left(&bench, GRANTEE), 0);
}

#[tokio::test]
async fn a_grant_is_judged_by_the_chain_of_the_city_it_is_addressed_to() {
    let (chain, ..) = city_chain();
    // In the other city, `RESIDENT` is the coordinator and `COORD` only a resident.
    let mut other = Chain::new(OTHER);
    other.resident(RESIDENT);
    other.appoint(OTHER, RESIDENT, true);
    other.resident(COORD);
    let bench = served(&[CITY, OTHER], EAGER, vec![(HOST, chain), (OTHER_HOST, other)]);
    let refused = (StatusCode::FORBIDDEN, "coordinator");
    is(deliver(&bench, COORD, Some(OTHER), now()).await, refused);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, refused);
    is(deliver(&bench, RESIDENT, Some(OTHER), now()).await, OK);
    is(deliver(&bench, COORD, Some(CITY), now()).await, OK);
}

#[tokio::test]
async fn the_old_list_still_counts() {
    let bench = bench_with(|config| config.cities = vec![did(CITY)]);
    bench.hosts.down.store(true, Ordering::SeqCst);
    let seen = capture(Level::Debug);
    // On the list, for no city or any: the host is not asked, and its being down is no matter.
    is(deliver(&bench, COORDINATOR, None, now()).await, OK);
    is(deliver(&bench, COORDINATOR, Some(CITY), now()).await, OK);
    assert_eq!(asked(&bench), 0);
    assert_eq!(seen.of("grant.admit")[0].text("via"), Some("list"));
    // With no city served, anyone else is refused as before.
    let bench = common::bench();
    is(deliver(&bench, COORD, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
}

#[tokio::test]
async fn a_host_that_cannot_be_reached_is_said_so_and_asked_again() {
    let (chain, ..) = city_chain();
    let bench = served(&[CITY], EAGER, vec![(HOST, chain)]);
    bench.hosts.down.store(true, Ordering::SeqCst);
    let seen = capture(Level::Debug);
    // A status pol's relay retries: neither 2xx nor 400.
    is(deliver(&bench, COORD, Some(CITY), now()).await, (StatusCode::BAD_GATEWAY, "city-host"));
    assert_eq!(seen.of("city.fetch")[0].text("code"), Some("city-host"));
    bench.hosts.down.store(false, Ordering::SeqCst);
    is(deliver(&bench, COORD, Some(CITY), now()).await, OK);
    // The directory, when a signer's document cannot be had.
    let (chain, ..) = city_chain();
    let bench = served(&[CITY], EAGER, vec![(HOST, chain)]);
    bench.keys.2.store(true, Ordering::SeqCst);
    is(deliver(&bench, COORD, Some(CITY), now()).await, (StatusCode::BAD_GATEWAY, "directory"));
}

#[tokio::test]
async fn a_failed_fetch_is_not_tried_again_at_once() {
    let (chain, ..) = city_chain();
    let bench = served(&[CITY], CityTimes::default(), vec![(HOST, chain)]);
    bench.hosts.down.store(true, Ordering::SeqCst);
    is(deliver(&bench, COORD, Some(CITY), now()).await, (StatusCode::BAD_GATEWAY, "city-host"));
    let before = asked(&bench);
    bench.hosts.down.store(false, Ordering::SeqCst);
    is(deliver(&bench, COORD, Some(CITY), now()).await, (StatusCode::BAD_GATEWAY, "city-host"));
    assert_eq!(asked(&bench), before);
}

#[tokio::test]
async fn a_city_without_a_host_is_said_so() {
    let bench = served(&[CITY], EAGER, vec![]);
    bench.keys.1.lock().unwrap().clear();
    is(deliver(&bench, COORD, Some(CITY), now()).await, (StatusCode::BAD_GATEWAY, "city-doc"));
    // A host that holds nothing for the city answers with no records: nobody is in office.
    let bench = served(&[CITY], EAGER, vec![]);
    publish(&bench, HOST, Chain::new(OTHER));
    is(deliver(&bench, COORD, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
}

#[tokio::test]
async fn a_new_coordinator_is_seen_before_the_chain_expires() {
    let (mut chain, ..) = city_chain();
    let mut held = Chain::new(CITY);
    held.held = chain.held.clone();
    let bench = served(&[CITY], EAGER, vec![(HOST, held)]);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
    // The one coordinator in office appoints the resident: 1 of 1, at once.
    chain.at = now() - 1000;
    chain.appoint(COORD, RESIDENT, true);
    publish(&bench, HOST, chain);
    let before = asked(&bench);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, OK);
    assert!(asked(&bench) > before, "the miss asked the host again");
    assert_eq!(left(&bench, GRANTEE), 2_500_000);
}

#[tokio::test]
async fn a_miss_asks_again_no_sooner_than_the_refetch_time() {
    let (mut chain, ..) = city_chain();
    let mut held = Chain::new(CITY);
    held.held = chain.held.clone();
    let bench = served(&[CITY], CityTimes::default(), vec![(HOST, held)]);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
    let before = asked(&bench);
    chain.at = now() - 1000;
    chain.appoint(COORD, RESIDENT, true);
    publish(&bench, HOST, chain);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
    assert_eq!(asked(&bench), before);
}

#[tokio::test]
async fn a_record_not_signed_by_its_author_is_left_out() {
    let (mut chain, ..) = city_chain();
    // The city's name on an appointment the resident signed with their own key.
    chain.signed(RESIDENT, CITY, RESIDENT, "coordinator", true, &[]);
    let bench = served(&[CITY], EAGER, vec![(HOST, chain)]);
    let seen = capture(Level::Debug);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
    assert_eq!(seen.of("city.fetch")[0].u64("discarded"), Some(1));
    // A body the host changed is not the one signed: the record has no effect.
    let (mut chain, ..) = city_chain();
    chain.add(COORD, RESIDENT, "coordinator", true, &[]);
    let appointment = chain.held.records.last().unwrap().body.unwrap();
    let changed =
        map(&[("who", Value::Str(did(RESIDENT))), ("kind", Value::Str("curator".into()))]);
    chain.held.bodies =
        BTreeMap::from_iter(chain.held.bodies.into_iter().filter(|(c, _)| *c != appointment));
    chain.held.bodies.insert(Cid::of(Codec::Drisl, &changed.encode()), changed);
    let bench = served(&[CITY], EAGER, vec![(HOST, chain)]);
    is(deliver(&bench, RESIDENT, Some(CITY), now()).await, (StatusCode::FORBIDDEN, "coordinator"));
}
