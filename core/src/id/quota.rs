//! Per-service UTC daily quota types, pure parsing/reset rules, and thin `POST /quota` fetch.

use crate::id::keys::{Ours, Service};
use crate::rt::supervise::End;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use ts_rs::TS;

/// One service's daily lease usage and remaining allowance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ServiceQuota {
    pub service: String,
    pub used: u32,
    pub limit: u32,
    pub remaining: u32,
}

/// Bounded per-service daily quota snapshot (always at most `Service::ALL.len()` entries).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Quota {
    #[ts(type = "number")]
    pub resets_at: i64,
    pub services: Vec<ServiceQuota>,
}

/// Pure parser for a `{ "resets_at", "services": { "soniox": ..., "openrouter": ... } }` object.
pub fn parse_quota(json: &Json) -> Option<Quota> {
    let resets_at = json.get("resets_at")?.as_i64()?;
    let services_obj = json.get("services")?.as_object()?;
    let mut services = Vec::with_capacity(Service::ALL.len());
    for service in Service::ALL {
        let Some(entry) = services_obj.get(service.name()) else {
            continue;
        };
        let used = u32::try_from(entry.get("used")?.as_u64()?).ok()?;
        let limit = u32::try_from(entry.get("limit")?.as_u64()?).ok()?;
        let remaining = u32::try_from(entry.get("remaining")?.as_u64()?).ok()?;
        services.push(ServiceQuota { service: service.name().to_owned(), used, limit, remaining });
    }
    if services.is_empty() {
        return None;
    }
    Some(Quota { resets_at, services })
}

/// Pure parser for a `POST /quota` response `{ "left", "resets_at", "services": ... }`.
pub fn parse(json: &Json) -> Option<(u64, Quota)> {
    let left = json.get("left")?.as_u64()?;
    let quota = parse_quota(json)?;
    Some((left, quota))
}

/// Remaining daily count for `service` at `now_ms`. Once `now_ms >= quota.resets_at` the UTC day
/// has rolled over so the full daily `limit` is available again.
pub fn remaining_for(quota: &Quota, service: Service, now_ms: i64) -> Option<u32> {
    let entry = quota.services.iter().find(|entry| entry.service == service.name())?;
    if now_ms >= quota.resets_at { Some(entry.limit) } else { Some(entry.remaining) }
}

/// Whether `service` has exhausted its daily count at `now_ms`. Past `resets_at` the count has
/// rolled over to a fresh UTC day and is no longer considered exhausted.
pub fn is_exhausted(quota: &Quota, service: Service, now_ms: i64) -> bool {
    if now_ms >= quota.resets_at {
        return false;
    }
    quota
        .services
        .iter()
        .find(|entry| entry.service == service.name())
        .is_some_and(|entry| entry.remaining == 0)
}

/// Fetches the signed-in resident's balance and per-service daily quota from `POST /quota`.
pub async fn fetch(ours: &Ours) -> Result<(u64, Quota), End> {
    let said = ours.ask("quota", "talk/quota", json!({})).await?;
    parse(&said).ok_or(End::Retry("server"))
}
