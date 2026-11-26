//! The ledger: one pack of signed records — coordinators' grants, the requests for keys people
//! signed, their claims of what was left on an invite of the 0.8 line — and, beside each claim,
//! what it carried over. A balance is their fold, in the order they were kept: a key costs what
//! it could spend, drawn on its asker's grant that ends first. Keys are small, so that what a
//! key could spend and what it did are close. Money is in µ$.

use crate::{Answer, BoxFuture, Delivery, Refusal, Server, field, lock, now};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
use std::{path::Path, sync::Arc};
use zu::{Cid, Codec, Pack, Record, Value};

const QUOTA: Refusal = Refusal(StatusCode::PAYMENT_REQUIRED, "quota");
const QUOTA_DAILY: Refusal = Refusal(StatusCode::PAYMENT_REQUIRED, "quota.daily");
const UPSTREAM: Refusal = Refusal(StatusCode::BAD_GATEWAY, "upstream");
const CLAIMED: Refusal = Refusal(StatusCode::CONFLICT, "claimed");
/// Milliseconds in one UTC calendar day.
pub const DAY_MS: i64 = 86_400_000;
/// Default daily Soniox key leases per resident (`Settings.dc.html`).
pub const DEFAULT_DAILY_SONIOX_TIMES: u32 = 40;
/// Default daily OpenRouter key leases per resident (`Settings.dc.html`).
pub const DEFAULT_DAILY_OPENROUTER_TIMES: u32 = 30;
/// How long an upstream (the old invite service, Soniox, OpenRouter) has to answer before the
/// asker is told `upstream`. Origin to be traced.
const UPSTREAM_WAIT_MS: u64 = 10_000;
/// How long an OpenRouter key lives before OpenRouter ends it. Origin to be traced.
const OPENROUTER_KEY_LIFE_MS: u64 = 3_600_000;

/// One service's daily usage and remaining count for a resident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceDaily {
    pub used: u32,
    pub limit: u32,
    pub remaining: u32,
}

/// Snapshot of a resident's remaining balance (µ$) and UTC per-service daily quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaReport {
    pub left: u64,
    pub resets_at: i64,
    pub soniox: ServiceDaily,
    pub openrouter: ServiceDaily,
}

/// Start and exclusive end (next UTC midnight) of the UTC day containing `now_ms`.
pub fn day_bounds(now_ms: i64) -> (i64, i64) {
    let start = now_ms.div_euclid(DAY_MS) * DAY_MS;
    (start, start + DAY_MS)
}

/// Pure rule for one service's daily allowance given today's usage, configured daily cap, whether
/// the resident holds any live grant/claim, and whether any balance remains.
pub fn daily_service(
    used: u32,
    daily_cap: u32,
    has_grant: bool,
    has_balance: bool,
) -> ServiceDaily {
    let limit = if has_grant { daily_cap } else { 0 };
    let remaining = if has_balance { limit.saturating_sub(used) } else { 0 };
    ServiceDaily { used, limit, remaining }
}

fn service_json(daily: ServiceDaily) -> serde_json::Value {
    json!({ "used": daily.used, "limit": daily.limit, "remaining": daily.remaining })
}

fn quota_json(report: &QuotaReport) -> serde_json::Value {
    json!({
        "resets_at": report.resets_at,
        "services": {
            "soniox": service_json(report.soniox),
            "openrouter": service_json(report.openrouter),
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    Soniox,
    OpenRouter,
}

/// A key as the upstream made it. Not `Debug`: the key goes to the asker and nowhere else.
pub struct Minted {
    pub key: String,
    /// The longest one connection made with the key may last.
    pub session_cap_s: Option<u64>,
}

/// One upstream: a row of the table the server keeps, behind this port.
pub trait Upstream: Send + Sync {
    /// What one key may spend, in µ$: its cost.
    fn slice(&self) -> u64;
    /// What a second of its stream costs, in µ$, if it is priced by time.
    fn per_second(&self) -> u64 {
        0
    }
    /// A key named `name` that can spend `micros` at most, and lives an hour at most.
    fn mint<'a>(&'a self, name: &'a str, micros: u64) -> BoxFuture<'a, Result<Minted, Refusal>>;
}

/// The service a request for a key names.
fn service(body: Option<&Value>) -> Option<Service> {
    field(body, "service").and_then(|service| serde_json::from_value(json!(service)).ok())
}

/// What a claim carried over, kept beside it: `{ claim, to, seconds }`, the seconds of live
/// captions the old service said the invite had left. It is the server's own word. Every body
/// anyone else delivers has another field (`cents`, `service`), so none passes for one.
fn credit(body: &Value) -> Option<(String, u64)> {
    let Value::Map(body) = body else { return None };
    match (body.len(), body.get("claim"), body.get("to"), body.get("seconds")) {
        (4, Some(Value::Link(_)), Some(Value::Str(to)), Some(&Value::Int(seconds))) => {
            Some((to.clone(), u64::try_from(seconds).ok()?))
        }
        _ => None,
    }
}

/// A grant's body, `{ to, cents, until }`: `to` may spend this many µ$ until `until`.
fn purse(body: Option<&Value>) -> Option<(String, u64, i64)> {
    let Some(Value::Map(body)) = body else { return None };
    match (body.get("to"), body.get("cents"), body.get("until")) {
        (Some(Value::Str(to)), Some(&Value::Int(cents)), Some(&Value::Int(until))) => {
            Some((to.clone(), u64::from(u32::try_from(cents).ok()?) * 10_000, until))
        }
        _ => None,
    }
}

pub struct Book {
    pack: Pack,
    records: Vec<Record>,
    bodies: BTreeMap<Cid, Value>,
    /// The cost of a key of each service.
    slices: [u64; 2],
    /// What a second of live captions costs: what a claim's seconds are worth.
    second: u64,
    /// Daily key-lease cap per service (`Service as usize`).
    daily: [u32; 2],
    /// Requests being answered: held already, so neither balance nor daily cap is drawn twice.
    pending: Vec<(Cid, String, Service, u64)>,
}

impl Book {
    pub fn open(
        path: &Path,
        slices: [u64; 2],
        second: u64,
        daily: [u32; 2],
    ) -> Result<Book, zu::Error> {
        let (pack, held) = Pack::open(path)?;
        let (records, bodies, pending) = (held.records, held.bodies, Vec::new());
        Ok(Book { pack, records, bodies, slices, second, daily, pending })
    }

    /// A record and its body, kept and on the disk before it counts.
    fn keep(&mut self, record: Record, body: Option<Value>) -> Result<(), zu::Error> {
        body.iter().try_for_each(|body| self.pack.keep(body).map(drop))?;
        self.pack.append(&record)?;
        self.pack.sync()?;
        self.bodies.extend(record.body.zip(body));
        self.records.push(record);
        Ok(())
    }

    /// A claim, kept without its body (the token), and beside it what it carried over.
    /// Whether the invite `token` names was credited already.
    fn claimed(&self, token: &Cid) -> bool {
        let token = Some(&Value::Link(*token));
        self.bodies.values().any(|body| matches!(body, Value::Map(m) if m.get("token") == token))
    }

    /// `token` is the claimed token's CID: the token itself is not kept.
    fn credit(&mut self, claim: Record, seconds: u32, token: Cid) -> Result<(), zu::Error> {
        let to = Value::Str(claim.path.by.clone());
        let fields = [
            ("claim", Value::Link(claim.cid())),
            ("to", to),
            ("seconds", Value::Int(seconds.into())),
            ("token", Value::Link(token)),
        ];
        let credit = Value::Map(fields.map(|(name, value)| (name.into(), value)).into());
        self.pack.append(&claim)?;
        let cid = self.pack.keep(&credit)?;
        self.pack.sync()?;
        self.bodies.insert(cid, credit);
        self.records.push(claim);
        Ok(())
    }

    /// The `at` of a record held here.
    pub fn at_of(&self, cid: &Cid) -> Option<i64> {
        self.records.iter().find(|record| record.cid() == *cid).map(|record| record.at)
    }

    /// Every grant as it stands (record.md § Which Record a Path Refers To: one its
    /// coordinator replaced or removed is no longer one), with what is left of it.
    fn fold(&self) -> BTreeMap<String, (String, u64, i64)> {
        let grants: Vec<Record> =
            self.records.iter().filter(|r| r.path.ty == "talk/grant").cloned().collect();
        let mut purses: BTreeMap<String, _> = zu::record::paths(&grants)
            .into_iter()
            .filter_map(|(path, now)| {
                Some((path.to_string(), purse(self.bodies.get(&now[0].body?))?))
            })
            .collect();
        // What a claim carried over does not end. It is worth what live captions cost now.
        let credits = self.bodies.iter().filter_map(|(cid, body)| Some((cid, credit(body)?)));
        purses.extend(credits.map(|(cid, (to, seconds))| {
            (cid.to_string(), (to, seconds.saturating_mul(self.second), i64::MAX))
        }));
        let leases = self.records.iter().filter(|r| r.path.ty == "talk/lease");
        let cost = |r: &Record| Some(self.slices[service(self.bodies.get(&r.body?))? as usize]);
        let leases = leases.map(|r| (r.path.by.clone(), r.at, cost(r).unwrap_or(0)));
        let pending = self.pending.iter().map(|(_, did, _, cost)| (did.clone(), now(), *cost));
        for (did, at, cost) in leases.chain(pending) {
            let mut live: Vec<_> =
                purses.values_mut().filter(|p| p.0 == did && p.2 > at && p.1 > 0).collect();
            live.sort_by_key(|purse| purse.2);
            if let Some(purse) = live.into_iter().next() {
                purse.1 -= purse.1.min(cost);
            }
        }
        purses
    }

    /// Leases already kept plus in-flight requests for `(did, svc)` in `[day_start, resets_at)`.
    fn used_on_day(&self, did: &str, svc: Service, day_start: i64, resets_at: i64) -> u32 {
        let kept = self
            .records
            .iter()
            .filter(|r| {
                r.path.ty == "talk/lease"
                    && r.path.by == did
                    && r.at >= day_start
                    && r.at < resets_at
                    && r.body.and_then(|cid| service(self.bodies.get(&cid))) == Some(svc)
            })
            .count();
        let now_ms = now();
        let inflight = if now_ms >= day_start && now_ms < resets_at {
            self.pending.iter().filter(|(_, who, s, _)| who == did && *s == svc).count()
        } else {
            0
        };
        u32::try_from(kept.saturating_add(inflight)).unwrap_or(u32::MAX)
    }

    /// What `did` has left at `now`, the grant that ends first first.
    pub fn free(&self, did: &str, now: i64) -> Vec<(String, u64)> {
        let mut live: Vec<_> =
            self.fold().into_iter().filter(|(_, p)| p.0 == did && p.2 > now).collect();
        live.sort_by_key(|(_, purse)| purse.2);
        live.into_iter().map(|(grant, purse)| (grant, purse.1)).collect()
    }

    /// Balance and per-service UTC daily quota for `did` at `now_ms`.
    pub fn quota(&self, did: &str, now_ms: i64) -> QuotaReport {
        let purses = self.fold();
        let mut left: u64 = 0;
        let mut has_grant = false;
        for p in purses.values() {
            if p.0 == did && p.2 > now_ms {
                has_grant = true;
                left = left.saturating_add(p.1);
            }
        }
        let (day_start, resets_at) = day_bounds(now_ms);
        let has_balance = left > 0;
        let soniox_used = self.used_on_day(did, Service::Soniox, day_start, resets_at);
        let openrouter_used = self.used_on_day(did, Service::OpenRouter, day_start, resets_at);
        QuotaReport {
            left,
            resets_at,
            soniox: daily_service(
                soniox_used,
                self.daily[Service::Soniox as usize],
                has_grant,
                has_balance,
            ),
            openrouter: daily_service(
                openrouter_used,
                self.daily[Service::OpenRouter as usize],
                has_grant,
                has_balance,
            ),
        }
    }
}

/// `POST /grants`: a `talk/grant` record and its body. Whoever holds one may deliver it; what
/// counts is that a coordinator signed it. Delivered again, it changes nothing.
pub async fn grant(State(server): State<Arc<Server>>, Json(sent): Json<Delivery>) -> Answer {
    let only = Some(&server.config.coordinators[..]);
    let (record, body) = server.check(sent, "talk/grant", only).await?;
    if record.body.is_some() && purse(body.as_ref()).is_none() {
        return Err(Refusal(StatusCode::BAD_REQUEST, "body"));
    }
    let (cid, mut book) = (record.cid(), lock(&server.book));
    if book.at_of(&cid).is_none() {
        book.keep(record, body)?;
    }
    Ok(Json(json!({ "cid": cid.to_string() })))
}

/// `POST /quota`: a `talk/quota` record `{ aud }`. Returns the resident's balance and per-service
/// UTC daily quota.
pub async fn quota(State(server): State<Arc<Server>>, Json(sent): Json<Delivery>) -> Answer {
    let (record, _) = server.check(sent, "talk/quota", None).await?;
    let report = server.quota(&record.path.by, now());
    Ok(Json(json!({
        "left": report.left,
        "resets_at": report.resets_at,
        "services": {
            "soniox": service_json(report.soniox),
            "openrouter": service_json(report.openrouter),
        }
    })))
}

/// `POST /lease`: a `talk/lease` record `{ service, aud }`. The answer is a key, what the
/// asker has left, and their updated daily quota. The request is kept only once the key is made.
pub async fn lease(State(server): State<Arc<Server>>, Json(sent): Json<Delivery>) -> Answer {
    let (record, body) = server.check(sent, "talk/lease", None).await?;
    let (cid, did) = (record.cid(), record.path.by.clone());
    let service = service(body.as_ref()).ok_or(Refusal(StatusCode::BAD_REQUEST, "service"))?;
    let upstream = server.upstreams[service as usize].clone();
    let cost = {
        let mut book = lock(&server.book);
        if book.at_of(&cid).is_some() || book.pending.iter().any(|(pending, ..)| *pending == cid) {
            return Err(Refusal(StatusCode::CONFLICT, "replay"));
        }
        let now_ms = now();
        let free = book.free(&did, now_ms).into_iter().map(|(_, free)| free).find(|free| *free > 0);
        let cost = free.ok_or(QUOTA)?.min(upstream.slice());
        let (day_start, resets_at) = day_bounds(now_ms);
        let used_today = book.used_on_day(&did, service, day_start, resets_at);
        if used_today >= book.daily[service as usize] {
            return Err(QUOTA_DAILY);
        }
        book.pending.push((cid, did.clone(), service, cost));
        cost
    };
    // On a supervised task of its own, finished even if the asker goes away: nothing stays held.
    let server_task = server.clone();
    let task = server.tasks.spawn("ledger.lease", async move {
        let minted = upstream.mint(&record.path.key, cost).await;
        let mut book = lock(&server_task.book);
        book.pending.retain(|(pending, ..)| *pending != cid);
        // Not written down, so not handed out: a key that was made but not kept dies at its
        // expiry, unused.
        let minted = minted?;
        book.keep(record, body)?;
        let report = book.quota(&did, now());
        Ok(Json(json!({
            "key": minted.key,
            "session_cap_s": minted.session_cap_s,
            "left": report.left,
            "quota": quota_json(&report),
        })))
    });
    task.await.unwrap_or(Err(Refusal(StatusCode::INTERNAL_SERVER_ERROR, "task")))
}

/// `POST /claim`: a `talk/claim` record `{ token, aud }`. The hours of live captions left on
/// a community invite of the 0.8 line become the asker's. The old service retires the invite
/// and keeps saying how much it had; this ledger credits each token once, by its CID, so a
/// claim cut short between the two is credited when it is sent again. The token is not kept.
pub async fn claim(State(server): State<Arc<Server>>, Json(sent): Json<Delivery>) -> Answer {
    let (record, body) = server.check(sent, "talk/claim", None).await?;
    let token = field(body.as_ref(), "token").ok_or(Refusal(StatusCode::BAD_REQUEST, "body"))?;
    let token = token.clone();
    // On a supervised task of its own: what the old service retired is credited, even if the asker goes.
    let server_task = server.clone();
    let task = server.tasks.spawn("ledger.claim", async move {
        let seconds = server_task.config.invites.as_ref().ok_or(UPSTREAM)?.retire(&token).await?;
        let (did, mut book) = (record.path.by.clone(), lock(&server_task.book));
        let token = Cid::of(Codec::Raw, token.as_bytes());
        if book.claimed(&token) {
            return Err(CLAIMED);
        }
        book.credit(record, seconds, token)?;
        let report = book.quota(&did, now());
        Ok(Json(json!({
            "seconds": seconds,
            "left": report.left,
            "quota": quota_json(&report),
        })))
    });
    task.await.unwrap_or(Err(Refusal(StatusCode::INTERNAL_SERVER_ERROR, "task")))
}

/// The 0.8 line's community-invite service, which a claim asks to retire its invite. Its admin
/// token is the operator's, as the upstreams' master keys are.
pub struct Invites {
    pub http: reqwest::Client,
    pub url: String,
    pub key: String,
}

impl Invites {
    /// Retires the invite `token` opens, and says how many seconds it had left. Asked again,
    /// the old service says the same number: whether it was credited is this ledger's to say.
    async fn retire(&self, token: &str) -> Result<u32, Refusal> {
        let request = self.http.post(format!("{}/v1/retire", self.url)).bearer_auth(&self.key);
        let request = request.json(&json!({ "access_token": token }));
        let answer = request
            .timeout(Duration::from_millis(UPSTREAM_WAIT_MS))
            .send()
            .await
            .map_err(|_| UPSTREAM)?;
        if answer.status() == StatusCode::NOT_FOUND {
            return Err(Refusal(StatusCode::NOT_FOUND, "invite"));
        }
        let answer = answer.error_for_status().map_err(|_| UPSTREAM)?;
        let said: serde_json::Value = answer.json().await.map_err(|_| UPSTREAM)?;
        let seconds = said["remaining_seconds"].as_u64().and_then(|s| u32::try_from(s).ok());
        match (said["status"].as_str(), seconds) {
            (Some("retired" | "already_retired"), Some(seconds)) => Ok(seconds),
            _ => Err(UPSTREAM),
        }
    }
}

/// Ask an upstream with its master key. Neither the key nor a failed answer goes anywhere.
async fn ask(request: reqwest::RequestBuilder, key: &str) -> Result<serde_json::Value, Refusal> {
    let answer =
        request.bearer_auth(key).timeout(Duration::from_millis(UPSTREAM_WAIT_MS)).send().await;
    let answer = answer.ok().filter(|answer| answer.status().is_success()).ok_or(UPSTREAM)?;
    answer.json().await.map_err(|_| UPSTREAM)
}

/// Soniox temporary keys for real-time transcription. Each opens one WebSocket, which Soniox
/// ends at the cap: the holder hands over to a fresh key at a pause before it.
pub struct Soniox {
    pub http: reqwest::Client,
    pub url: String,
    pub key: String,
    pub micros_per_s: u64,
    /// The longest connection, in seconds.
    pub cap_s: u64,
}

impl Upstream for Soniox {
    fn slice(&self) -> u64 {
        self.cap_s * self.micros_per_s
    }

    fn per_second(&self) -> u64 {
        self.micros_per_s
    }

    fn mint<'a>(&'a self, name: &'a str, micros: u64) -> BoxFuture<'a, Result<Minted, Refusal>> {
        Box::pin(async move {
            let cap = (micros / self.micros_per_s.max(1)).min(self.cap_s);
            if cap == 0 {
                return Err(QUOTA);
            }
            let body = json!({ "usage_type": "transcribe_websocket", "single_use": true,
                "client_reference_id": name, "expires_in_seconds": 300,
                "max_session_duration_seconds": cap });
            let url = format!("{}/v1/auth/temporary-api-key", self.url);
            let made = ask(self.http.post(url).json(&body), &self.key).await?;
            let key = made["api_key"].as_str().ok_or(UPSTREAM)?.to_owned();
            Ok(Minted { key, session_cap_s: Some(cap) })
        })
    }
}

/// OpenRouter keys made with a management key: a limit in dollars, and an expiry.
pub struct OpenRouter {
    pub http: reqwest::Client,
    pub url: String,
    pub key: String,
    pub slice: u64,
}

impl Upstream for OpenRouter {
    fn slice(&self) -> u64 {
        self.slice
    }

    fn mint<'a>(&'a self, name: &'a str, micros: u64) -> BoxFuture<'a, Result<Minted, Refusal>> {
        Box::pin(async move {
            let expires = SystemTime::now() + Duration::from_millis(OPENROUTER_KEY_LIFE_MS);
            let expires = humantime::format_rfc3339_seconds(expires).to_string();
            let body = json!({ "name": name, "limit": micros as f64 / 1e6, "expires_at": expires });
            let made = ask(self.http.post(format!("{}/keys", self.url)).json(&body), &self.key);
            let key = made.await?["key"].as_str().ok_or(UPSTREAM)?.to_owned();
            Ok(Minted { key, session_cap_s: None })
        })
    }
}
