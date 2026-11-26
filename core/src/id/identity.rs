//! Identity flows: explicit [`AuthFlow`] state machine, handle normalization, two-step QR
//! sign-in confirmation (`DECISIONS.md` #5), identity creation with handle, recovery via rotation
//! key, and offline handle caching. Secrets stay in `Zeroizing` buffers and never reach logs or
//! `State`.

use crate::capture::clock::wall_ms;
use crate::id::keys::{
    self, CREATE_WAIT_MS, Me, SIGN_IN_OFFLINE_TRIES_TIMES, SIGN_IN_POLL_MS, SIGN_IN_WAIT_MS, SignIn,
};
use crate::rt::supervise::End;
use reqwest::{Client, StatusCode, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use ts_rs::TS;
use zeroize::Zeroizing;
use zu::{SecretKey, did};

/// Default domain suffix appended when a person enters a short handle label (`P4.md` §3.4).
pub const DEFAULT_HANDLE_DOMAIN: &str = "zuzalu.ink";
/// Minimum byte length of a short handle label (`Recovery.dc.html`).
pub const HANDLE_MIN_BYTES: usize = 2;
/// Maximum byte length of a short handle label (`Recovery.dc.html`).
pub const HANDLE_MAX_BYTES: usize = 32;
/// Timeout for fetching a DID audit log or document from its directory.
pub const DIRECTORY_WAIT_MS: u64 = 15_000;

/// Explicit state machine for an in-flight identity flow (`P4.md` §3.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "is")]
pub enum AuthFlow {
    #[default]
    Idle,
    Scanning {
        url: String,
        #[ts(type = "number")]
        expires_at: i64,
    },
    Confirm {
        did: String,
        handle: Option<String>,
        /// Reserved for future 6-digit verification once zu login.md specifies it (DECISIONS.md #5).
        code: Option<String>,
    },
    Confirming {
        did: String,
        handle: Option<String>,
    },
    Creating,
    Recovering,
}

impl AuthFlow {
    /// Stable state code for `identity.auth` structured log events (never carries a DID or URL).
    pub const fn code(&self) -> &'static str {
        match self {
            AuthFlow::Idle => "Idle",
            AuthFlow::Scanning { .. } => "Scanning",
            AuthFlow::Confirm { .. } => "Confirm",
            AuthFlow::Confirming { .. } => "Confirming",
            AuthFlow::Creating => "Creating",
            AuthFlow::Recovering => "Recovering",
        }
    }
}

/// Transition input for [`auth_step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    BeginScan { url: String, expires_at: i64 },
    Received { did: String, handle: Option<String>, code: Option<String> },
    BeginConfirm,
    BeginCreate,
    BeginRecover,
    Completed,
    Cancelled,
    Failed,
}

/// Pure transition table for [`AuthFlow`].
pub fn auth_step(flow: &AuthFlow, event: AuthEvent) -> AuthFlow {
    match (flow, event) {
        (
            AuthFlow::Idle | AuthFlow::Scanning { .. } | AuthFlow::Confirm { .. },
            AuthEvent::BeginScan { url, expires_at },
        ) => AuthFlow::Scanning { url, expires_at },
        (AuthFlow::Scanning { .. }, AuthEvent::Received { did, handle, code }) => {
            AuthFlow::Confirm { did, handle, code }
        }
        (AuthFlow::Confirm { did, handle, .. }, AuthEvent::BeginConfirm) => {
            AuthFlow::Confirming { did: did.clone(), handle: handle.clone() }
        }
        (
            AuthFlow::Idle | AuthFlow::Scanning { .. } | AuthFlow::Confirm { .. },
            AuthEvent::BeginCreate,
        ) => AuthFlow::Creating,
        (
            AuthFlow::Idle | AuthFlow::Scanning { .. } | AuthFlow::Confirm { .. },
            AuthEvent::BeginRecover,
        ) => AuthFlow::Recovering,
        (_, AuthEvent::Completed | AuthEvent::Cancelled | AuthEvent::Failed) => AuthFlow::Idle,
        (same, _) => same.clone(),
    }
}

/// Normalizes an optional handle input:
/// - blank/whitespace -> `Ok(None)`
/// - leading `@` stripped and lowercased
/// - dotted name validated with `zu::did::handle`
/// - short label (2–32 ASCII `[a-z0-9-]`, not leading/trailing `-`) suffixed with
///   [`DEFAULT_HANDLE_DOMAIN`].
pub fn normalize_handle(input: &str) -> Result<Option<String>, &'static str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let bare = trimmed.strip_prefix('@').unwrap_or(trimmed).to_ascii_lowercase();
    if bare.contains('.') {
        return did::handle(&bare).map(Some).ok_or("id.handle");
    }
    let valid_len = (HANDLE_MIN_BYTES..=HANDLE_MAX_BYTES).contains(&bare.len());
    let valid_chars =
        bare.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid_len || !valid_chars || bare.starts_with('-') || bare.ends_with('-') {
        return Err("id.handle");
    }
    let full = format!("{bare}.{DEFAULT_HANDLE_DOMAIN}");
    did::handle(&full).map(Some).ok_or("id.handle")
}

/// Resolves the HTTP URL to fetch a DID's audit log (`did:zu:`) or document (`did:web:`),
/// routing `did:zu:` through `directory` so tests can point at a local stand-in directory.
pub fn did_doc_url(did_str: &str, directory: &str) -> Result<String, End> {
    let canonical = did::url(did_str, false).map_err(|_| End::Fatal("id.did"))?;
    if did_str.starts_with("did:zu:") {
        let base = directory.trim_end_matches('/');
        Ok(format!("{base}/{did_str}/log/audit"))
    } else {
        Ok(canonical)
    }
}

/// Verified sign-in payload waiting for the person to confirm before it is written to disk.
/// All sensitive fields are hidden from `Debug` and the signing seed is zeroized on drop.
pub struct PendingSignIn {
    did: String,
    handle: Option<String>,
    seed: Zeroizing<[u8; 32]>,
    code: Option<String>,
}

impl fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PendingSignIn { .. }")
    }
}

impl PendingSignIn {
    pub fn did(&self) -> &str {
        &self.did
    }

    pub fn handle(&self) -> Option<&str> {
        self.handle.as_deref()
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Persists the verified identity on this device once the person confirms.
    pub fn commit(self) -> Result<(Me, Option<String>), End> {
        let me = keys::keep_me(&self.seed, self.did)?;
        Ok((me, self.handle))
    }
}

/// Polls the sign-in relay, decrypts the sealed seed, verifies it against the DID's document or
/// audit log, and returns a [`PendingSignIn`] without writing anything to the key store yet.
pub async fn receive_sign_in(
    sign_in: &SignIn,
    http: &Client,
    directory: &str,
) -> Result<PendingSignIn, End> {
    let (deadline, offline) = (wall_ms() + SIGN_IN_WAIT_MS, |_| End::Retry("offline"));
    let mut offline_tries = 0u32;
    let sealed = loop {
        match http.get(sign_in.relay_room()).send().await {
            Ok(reply) => {
                offline_tries = 0;
                let status = reply.status();
                if status == StatusCode::OK {
                    break reply.bytes().await.map_err(offline)?;
                }
                let transient = status == StatusCode::NOT_FOUND
                    || status == StatusCode::REQUEST_TIMEOUT
                    || status == StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error();
                if !transient || wall_ms() >= deadline {
                    return Err(End::Fatal("id.expired"));
                }
            }
            Err(err) => {
                if offline_tries >= SIGN_IN_OFFLINE_TRIES_TIMES || wall_ms() >= deadline {
                    return Err(offline(err));
                }
                offline_tries += 1;
            }
        }
        tokio::time::sleep(Duration::from_millis(SIGN_IN_POLL_MS)).await;
    };
    let received = sign_in.login_code().open(&sealed, None).map_err(|_| End::Fatal("id.sealed"))?;
    let url = did_doc_url(&received.did, directory)?;
    let reply = http
        .get(url)
        .timeout(Duration::from_millis(DIRECTORY_WAIT_MS))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(offline)?;
    let bytes = reply.bytes().await.map_err(offline)?;
    let doc = did::resolve(&received.did, &bytes).map_err(|_| End::Fatal("id.did"))?;
    let seed_ref = received.seed(&doc).map_err(|_| End::Fatal("id.key"))?;
    let seed = Zeroizing::new(*seed_ref);
    let handle = doc.handles.into_iter().next();
    Ok(PendingSignIn { did: received.did.clone(), handle, seed, code: None })
}

/// Creates a new `did:zu` with an optional handle, registers its genesis operation at `directory`,
/// keeps the rotation key as unsaved for later backup reminder, and persists the identity.
pub async fn create_identity(
    http: &Client,
    directory: &str,
    handle: Option<String>,
) -> Result<(Me, Option<String>), End> {
    let normalized = match handle {
        Some(raw) => normalize_handle(&raw).map_err(End::Fatal)?,
        None => None,
    };
    let (seed, secret) =
        (Zeroizing::new(keys::random::<32>()?), Zeroizing::new(keys::random::<32>()?));
    let rotation = did::Rotation::from_bytes(&secret).map_err(|_| End::Retry("keys.random"))?;
    let aka: Vec<String> = normalized.iter().map(|h| format!("zu://{h}")).collect();
    let (did_str, op) =
        did::create_with_aka(&rotation, &SecretKey::from_seed(&seed).public(), &aka)
            .map_err(|_| End::Fatal("id.sign"))?;
    let base = directory.trim_end_matches('/');
    let sent = http
        .post(format!("{base}/{did_str}"))
        .header(CONTENT_TYPE, "application/json")
        .body(op)
        .timeout(Duration::from_millis(CREATE_WAIT_MS))
        .send()
        .await;
    match sent.map_err(|_| End::Retry("offline"))?.status() {
        status if status.is_success() => {}
        status if status.is_server_error() => return Err(End::Retry("server")),
        _ => return Err(End::Fatal("id.directory")),
    }
    keys::save_unsaved_rotation(&did_str, &secret)?;
    let me = keys::keep_me(&seed, did_str)?;
    Ok((me, normalized))
}

/// Recovers an existing `did:zu` using its P-256 rotation multikey by appending a fresh device
/// signing key to the directory log. Persists the new signing key only after the directory
/// accepts the update.
pub async fn recover_identity(
    http: &Client,
    directory: &str,
    did_str: &str,
    key: Zeroizing<String>,
) -> Result<(Me, Option<String>), End> {
    let did_trimmed = did_str.trim();
    if !did_trimmed.starts_with("did:zu:") {
        return Err(End::Fatal("id.did"));
    }
    let url = did_doc_url(did_trimmed, directory)?;
    let rotation =
        did::Rotation::from_multikey(key.trim()).map_err(|_| End::Fatal("id.recovery_key"))?;
    let reply = http
        .get(url)
        .timeout(Duration::from_millis(DIRECTORY_WAIT_MS))
        .send()
        .await
        .map_err(|_| End::Retry("offline"))?;
    match reply.status() {
        status if status.is_success() => {}
        status if status.is_server_error() => return Err(End::Retry("server")),
        _ => return Err(End::Fatal("id.did")),
    }
    let audit_bytes = reply.bytes().await.map_err(|_| End::Retry("offline"))?;
    let seed = Zeroizing::new(keys::random::<32>()?);
    let new_pub = SecretKey::from_seed(&seed).public();
    let (op, handles) = did::add_signing_key(did_trimmed, &audit_bytes, &rotation, &new_pub)
        .map_err(|err| match err {
            zu::Error::Signer => End::Fatal("id.recovery_key"),
            _ => End::Fatal("id.did"),
        })?;
    let base = directory.trim_end_matches('/');
    let sent = http
        .post(format!("{base}/{did_trimmed}"))
        .header(CONTENT_TYPE, "application/json")
        .body(op)
        .timeout(Duration::from_millis(CREATE_WAIT_MS))
        .send()
        .await;
    match sent.map_err(|_| End::Retry("offline"))?.status() {
        status if status.is_success() => {}
        status if status.is_server_error() => return Err(End::Retry("server")),
        _ => return Err(End::Fatal("id.directory")),
    }
    let me = keys::keep_me(&seed, did_trimmed.to_owned())?;
    Ok((me, handles.into_iter().next()))
}

/// Fetches the primary handle currently claimed by `did_str` from its directory or domain.
pub async fn fetch_handle(
    http: &Client,
    directory: &str,
    did_str: &str,
) -> Result<Option<String>, End> {
    let url = did_doc_url(did_str, directory)?;
    let reply = http
        .get(url)
        .timeout(Duration::from_millis(DIRECTORY_WAIT_MS))
        .send()
        .await
        .map_err(|_| End::Retry("offline"))?;
    match reply.status() {
        status if status.is_success() => {}
        status if status.is_server_error() => return Err(End::Retry("server")),
        _ => return Err(End::Fatal("id.did")),
    }
    let bytes = reply.bytes().await.map_err(|_| End::Retry("offline"))?;
    let doc = did::resolve(did_str, &bytes).map_err(|_| End::Fatal("id.did"))?;
    Ok(doc.handles.into_iter().next())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfileCache {
    handle: Option<String>,
}

/// Reads the offline-cached handle stored beside a person's shelf (`profile.json`).
pub fn read_cached_handle(shelf_root: &Path) -> Option<String> {
    let bytes = fs::read(shelf_root.join("profile.json")).ok()?;
    serde_json::from_slice::<ProfileCache>(&bytes).ok()?.handle
}

/// Atomically writes the person's handle (never any secret) beside their shelf (`profile.json`).
pub fn write_cached_handle(shelf_root: &Path, handle: Option<&str>) -> Result<(), End> {
    fs::create_dir_all(shelf_root).map_err(|_| End::Fatal("disk"))?;
    let payload = serde_json::to_vec(&ProfileCache { handle: handle.map(str::to_owned) })
        .map_err(|_| End::Fatal("disk"))?;
    let tmp = shelf_root.join("profile.json.tmp");
    let mut file = fs::File::create(&tmp).map_err(|_| End::Fatal("disk"))?;
    file.write_all(&payload).and_then(|()| file.sync_all()).map_err(|_| End::Fatal("disk"))?;
    fs::rename(tmp, shelf_root.join("profile.json")).map_err(|_| End::Fatal("disk"))
}
