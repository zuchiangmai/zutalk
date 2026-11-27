//! city.md § Working Out the State, against pol's `vectors/city.json` (`fixtures/city/`): the
//! same records give the same chain, forks, state, office terms and answers as pol's own code.

mod common;

use common::hex;
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap};
use zu::city::{self, Fold, Governance, Invitation, Role};
use zu::{Cid, PublicKey, Record, Value};

fn vectors() -> Json {
    let path = format!("{}/tests/fixtures/city/city.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text(json: &Json) -> &str {
    json.as_str().unwrap()
}

fn record(json: &Json) -> Record {
    let record = Record::try_from(Value::decode(&hex(text(&json["hex"]))).unwrap()).unwrap();
    assert_eq!(record.cid().to_string(), text(&json["cid"]));
    record
}

/// A body as a reader gets it: `None` if it did not come or is not DRISL.
fn body(case: &Json, record: &Record) -> Option<Value> {
    let cid = record.body?.to_string();
    Value::decode(&hex(case["bodies"][cid].as_str()?)).ok()
}

#[test]
fn every_case_of_pols_vectors() {
    let all = vectors();
    let city = text(&all["city"]);
    let people: Vec<&str> = all["people"].as_object().unwrap().values().map(text).collect();
    let keys: HashMap<&str, PublicKey> = all["keys"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(did, keys)| (did.as_str(), text(&keys[0]).parse().unwrap()))
        .collect();
    let cases = all["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 14);
    for case in cases {
        let name = text(&case["name"]);
        let list = |field: &str| case[field].as_array().unwrap().iter().map(record).collect();
        let (records, invites): (Vec<Record>, Vec<Record>) = (list("records"), list("invitations"));

        // Every record is a real one, signed with its DID's key.
        let ats: HashMap<Cid, i64> =
            records.iter().chain(&invites).map(|r| (r.cid(), r.at)).collect();
        for r in records.iter().chain(&invites) {
            let key_at = |by: &str, key: &PublicKey, _| keys.get(by) == Some(key);
            r.verify(key_at, |cid| ats.get(cid).copied()).unwrap();
        }

        let invitations: Vec<Invitation> =
            invites.iter().filter_map(|r| Invitation::new(r, &body(case, r)?)).collect();
        let steps = records.iter().map(|r| Governance::new(r.clone(), body(case, r).as_ref()));
        let chain = city::order(steps.collect());
        let ids = |list: &[Governance]| list.iter().map(|g| g.cid.to_string()).collect::<Vec<_>>();
        let strings = |field: &str| -> Vec<String> {
            case[field].as_array().unwrap().iter().map(|s| text(s).to_owned()).collect()
        };
        assert_eq!(ids(&chain.chain), strings("chain"), "{name}: chain");
        assert_eq!(ids(&chain.forks), strings("forks"), "{name}: forks");

        let state = city::state(city, &chain.chain, &invitations);
        let want = &case["state"];
        let residents: Vec<&str> = want["residents"].as_array().unwrap().iter().map(text).collect();
        assert_eq!(state.residents.iter().map(String::as_str).collect::<Vec<_>>(), residents);
        let roles: BTreeMap<&str, Vec<&str>> = state
            .roles
            .iter()
            .filter(|(_, held)| !held.is_empty())
            .map(|(who, held)| (who.as_str(), held.iter().map(|r| r.name()).collect()))
            .collect();
        let want_roles: BTreeMap<&str, Vec<&str>> = want["roles"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(who, held)| (who.as_str(), held.as_array().unwrap().iter().map(text).collect()))
            .collect();
        assert_eq!(roles, want_roles, "{name}: roles");
        let mut proposals: Vec<_> = state.proposals().collect();
        proposals.sort_by_key(|p| (p.at, p.opened));
        let said: Vec<Json> = proposals
            .iter()
            .map(|p| {
                serde_json::json!({ "opened": p.opened.to_string(), "who": p.who, "ok": p.ok,
                    "at": p.at, "n": p.n, "votes": p.votes })
            })
            .collect();
        assert_eq!(Json::Array(said), want["proposals"], "{name}: proposals");

        let terms = city::terms(city, &chain.chain, &invitations);
        let mut said: Vec<String> = terms
            .iter()
            .map(|t| format!("{} {} {} {:?}", t.from, t.who, t.role.name(), t.until))
            .collect();
        let mut want_terms: Vec<String> = case["terms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let (who, role) = (text(&t["who"]), text(&t["role"]));
                format!("{} {who} {role} {:?}", t["from"], t["until"].as_i64())
            })
            .collect();
        said.sort();
        want_terms.sort();
        assert_eq!(said, want_terms, "{name}: terms");

        for at in case["at"].as_array().unwrap() {
            let t = at["t"].as_i64().unwrap();
            let mut fold = Fold::new(city);
            chain
                .chain
                .iter()
                .filter(|g| g.record.at <= t)
                .for_each(|g| fold.apply(g, &invitations));
            for did in &people {
                let mut kinds: Vec<&str> = Vec::new();
                if fold.residents.contains(*did) {
                    kinds.push("resident");
                }
                for role in Role::ALL {
                    assert_eq!(fold.holds(did, role), city::held_at(&terms, did, role, t));
                    if city::held_at(&terms, did, role, t) {
                        kinds.push(role.name());
                    }
                }
                let want: Vec<&str> = at["holds"][*did]
                    .as_array()
                    .map_or(vec![], |kinds| kinds.iter().map(text).collect());
                assert_eq!(kinds, want, "{name}: {did} at {t}");
            }
        }
    }
}

#[test]
fn no_records_make_an_empty_city() {
    let chain = city::order(vec![]);
    assert!(chain.chain.is_empty() && chain.forks.is_empty());
    let state = city::state("did:zu:cccccccccccccccccccccccc", &chain.chain, &[]);
    assert!(state.residents.is_empty() && state.proposals().next().is_none());
    assert!(city::terms("did:zu:cccccccccccccccccccccccc", &chain.chain, &[]).is_empty());
}

#[test]
fn a_record_given_twice_counts_once() {
    let all = vectors();
    let case = &all["cases"][0];
    let records: Vec<Record> = case["records"].as_array().unwrap().iter().map(record).collect();
    let steps = |records: &[Record]| -> Vec<Governance> {
        records.iter().map(|r| Governance::new(r.clone(), body(case, r).as_ref())).collect()
    };
    let twice: Vec<Record> = records.iter().chain(&records).cloned().collect();
    assert_eq!(city::order(steps(&twice)), city::order(steps(&records)));
    assert!(city::order(steps(&twice)).forks.is_empty());
}
