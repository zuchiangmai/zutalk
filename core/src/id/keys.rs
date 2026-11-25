//! Keys, and whose they are. The person's Zu identity, received from a device that holds it
//! already (login.md) or made here, which signs every request to our server; the services'
//! keys, the person's own or short-lived ones our server draws from granted quota; the keys
//! each recording's audio and each room is sealed with. Secrets live in one place, and do not
//! print.

use crate::capture::clock::wall_ms;
use crate::id::quota::{self, Quota};
use crate::rt::supervise::End;
use base64::prelude::{BASE64_STANDARD, Engine};
use keyring_core::{Entry, Error};
use reqwest::{Client, StatusCode, header::CONTENT_TYPE};
use serde::Serialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::{fmt, fs, time::Duration};
use ts_rs::TS;
use zeroize::Zeroizing;
use zu::{Cid, Codec, Record, SecretKey, Value, did, login};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    Soniox,
    OpenRouter,
}

impl Service {
    pub const ALL: [Service; 2] = [Service::Soniox, Service::OpenRouter];

    pub fn name(self) -> &'static str {
        ["soniox", "openrouter"][self as usize]
    }
}

/// A secret that does not print.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// A key to open one connection, or make requests, with; and the longest one connection made
/// with it may last, when the issuer set a limit.
#[derive(Debug, Clone)]
pub struct Lease {
    pub key: Secret,
    pub session_cap: Option<Duration>,
}

pub trait Keys: Send + Sync {
    /// `End::Fatal` when there is no key to be had; `End::Retry` when asking again may help.
    fn lease(&self, service: Service) -> impl Future<Output = Result<Lease, End>> + Send;
    /// The service refused the key just leased: do not hand the same one out again.
    fn refused(&self, service: Service);
}

/// Where secrets live when it is not the system's key store: a folder only this account may
/// enter, holding one file only it may read. macOS keeps them there while releases are ad-hoc
/// signed, since each update changes the signature and the keychain then asks for a password.
static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The secrets in the file, each name with its secret in base64.
type Kept = Zeroizing<Vec<(String, String)>>;

/// Opens where this device keeps secrets: on macOS the file under `data`, elsewhere the
/// system's key store. Call once, before anything is kept or read.
pub fn open_store(data: &Path) -> Result<(), End> {
    if cfg!(target_os = "macos") {
        *FILE.lock().map_err(|_| End::Fatal("keys.store"))? = Some(data.join("Secrets"));
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new();
    #[cfg(target_os = "linux")]
    let store = dbus_secret_service_keyring_store::Store::new();
    #[cfg(not(target_os = "macos"))]
    keyring_core::set_default_store(store.map_err(|_| End::Fatal("keys.store"))?);
    Ok(())
}

fn read(dir: &Path) -> Result<Kept, End> {
    let json = match fs::read(dir.join("secrets.json")) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Kept::default()),
        read => Zeroizing::new(read.map_err(|_| End::Fatal("keys.store"))?),
    };
    serde_json::from_slice(&json).map(Zeroizing::new).map_err(|_| End::Fatal("keys.store"))
}

/// Written whole beside the file, then moved over it: a crash leaves the old or the new. The
/// folder is closed to others first, so that nobody can open the new file meanwhile.
fn write(dir: &Path, kept: &Kept) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    private(dir, 0o700)?;
    let (json, new) = (Zeroizing::new(serde_json::to_vec(&**kept)?), dir.join("secrets.new"));
    let mut file = fs::File::create(&new)?;
    private(&new, 0o600)?;
    file.write_all(&json).and_then(|()| file.sync_all())?;
    fs::rename(new, dir.join("secrets.json"))
}

/// Only this account may open `path`. Only macOS keeps the file, so other systems need not.
#[cfg_attr(not(unix), allow(unused_variables))]
fn private(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
    Ok(())
}

/// A secret kept under `name`, if there is one.
pub fn get(name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, End> {
    if let Some(dir) = FILE.lock().map_err(|_| End::Fatal("keys.store"))?.as_deref() {
        let kept = read(dir)?;
        let found = kept.iter().find(|(kept, _)| kept == name).map(|(_, secret)| secret);
        let secret = found.map(|secret| BASE64_STANDARD.decode(secret).map(Zeroizing::new));
        return secret.transpose().map_err(|_| End::Fatal("keys.store"));
    }
    let entry = Entry::new("ZuTalk", name).map_err(|_| End::Fatal("keys.store"))?;
    match entry.get_secret() {
        Ok(secret) => Ok(Some(Zeroizing::new(secret))),
        Err(Error::NoEntry) => Ok(None),
        Err(_) => Err(End::Fatal("keys.store")),
    }
}

/// Keeps `secret` under `name`, or forgets it.
pub fn set(name: &str, secret: Option<&[u8]>) -> Result<(), End> {
    // Held until the file is written: two changes at once would lose one.
    if let Some(dir) = FILE.lock().map_err(|_| End::Fatal("keys.store"))?.as_deref() {
        let mut kept = read(dir)?;
        kept.retain(|(kept, _)| kept != name);
        kept.extend(secret.map(|secret| (name.to_owned(), BASE64_STANDARD.encode(secret))));
        return write(dir, &kept).map_err(|_| End::Fatal("keys.store"));
    }
    let entry = Entry::new("ZuTalk", name).map_err(|_| End::Fatal("keys.store"))?;
    let done = match secret {
        Some(secret) => entry.set_secret(secret),
        None => entry
            .delete_credential()
            .or_else(|e| if let Error::NoEntry = e { Ok(()) } else { Err(e) }),
    };
    done.map_err(|_| End::Fatal("keys.store"))
}

pub fn random<const N: usize>() -> Result<[u8; N], End> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| End::Fatal("keys.random"))?;
    Ok(bytes)
}

/// A key nobody can guess, for a room or a record (path.md asks for random keys): 128 bits.
pub fn fresh() -> Result<String, End> {
    Ok(format!("{:032x}", u128::from_be_bytes(random()?)))
}

/// The key one thing is sealed with: a recording's audio (`tape`), a room (`room`). Each is
/// its own, drawn from one secret per purpose this device keeps, so there is one secret to
/// keep however many there are.
pub fn derive(purpose: &str, id: &str) -> Result<[u8; 32], End> {
    let secret = match get(purpose)? {
        Some(secret) => secret,
        None => {
            let fresh = Zeroizing::new(random::<32>()?.to_vec());
            set(purpose, Some(&fresh))?;
            fresh
        }
    };
    let mut key = [0; 32];
    let drawn = hkdf::Hkdf::<Sha256>::new(None, &secret).expand(id.as_bytes(), &mut key);
    drawn.map_err(|_| End::Fatal("keys.derive"))?;
    Ok(key)
}

/// A person signed in on this device. Their key signs records and requests alike.
#[derive(Clone)]
pub struct Me {
    pub did: String,
    pub key: Arc<SecretKey>,
}

/// The identity kept on this device, if any: its 32-byte seed, then its DID.
pub fn kept() -> Result<Option<Me>, End> {
    let Some(kept) = get("identity")? else { return Ok(None) };
    let (seed, did) = kept.split_at_checked(32).ok_or(End::Fatal("id.kept"))?;
    let did = String::from_utf8(did.to_vec()).map_err(|_| End::Fatal("id.kept"))?;
    let seed: &[u8; 32] = seed.try_into().map_err(|_| End::Fatal("id.kept"))?;
    Ok(Some(Me { did, key: Arc::new(SecretKey::from_seed(seed)) }))
}

/// Keeps an identity on this device, as [`kept`] reads it.
pub(crate) fn keep_me(seed: &[u8; 32], did: String) -> Result<Me, End> {
    keep(seed, did)
}

/// Keeps an identity on this device, as [`kept`] reads it.
fn keep(seed: &[u8; 32], did: String) -> Result<Me, End> {
    set("identity", Some(&Zeroizing::new([seed.as_slice(), did.as_bytes()].concat())))?;
    Ok(Me { did, key: Arc::new(SecretKey::from_seed(seed)) })
}

/// Saves a newly created identity's rotation key until the person confirms they backed it up.
pub(crate) fn save_unsaved_rotation(secret: &[u8; 32]) -> Result<(), End> {
    set(UNSAVED, Some(secret.as_slice()))
}

/// The device's own identity if one was already made on this device, without creating a new seed.
pub fn local_kept() -> Result<Option<Me>, End> {
    let Some(kept) = get("identity.local")? else { return Ok(None) };
    let seed: [u8; 32] = kept.as_slice().try_into().map_err(|_| End::Fatal("keys.store"))?;
    let seed = Zeroizing::new(seed);
    let key = Arc::new(SecretKey::from_seed(&seed));
    let did = did::local(&key.public());
    Ok(Some(Me { did, key }))
}

/// The device's own identity, made the first time it is asked for and kept with the secrets
/// as `identity.local` (a 32-byte seed). No network. `true`: it was made just now.
pub fn local() -> Result<(Me, bool), End> {
    let (seed, made) = match get("identity.local")? {
        Some(kept) => {
            let seed: [u8; 32] =
                kept.as_slice().try_into().map_err(|_| End::Fatal("keys.store"))?;
            (Zeroizing::new(seed), false)
        }
        None => {
            let seed = Zeroizing::new(random::<32>()?);
            set("identity.local", Some(seed.as_slice()))?;
            (seed, true)
        }
    };
    let key = Arc::new(SecretKey::from_seed(&seed));
    let did = did::local(&key.public());
    Ok((Me { did, key }, made))
}

/// Who signs on this device now. A recording is signed to its end by whoever signed its start.
pub enum Who {
    /// The secrets cannot be read or kept: nothing can be signed; the code says why.
    Nobody(&'static str),
    /// Nobody is signed in: the device's own identity.
    Local(Me),
    /// The person signed in: their Zu DID.
    Person(Me),
}

impl Who {
    /// Pure transition: the signed-in person, else the device's own identity, else nobody.
    pub fn of(kept: Option<Me>, local: Result<Me, End>) -> Who {
        if let Some(me) = kept {
            return Who::Person(me);
        }
        match local {
            Ok(me) => Who::Local(me),
            Err(End::Retry(code) | End::Fatal(code)) => Who::Nobody(code),
            Err(End::Done) => Who::Nobody("keys.store"),
        }
    }

    /// The state as the log says it: never a DID.
    pub fn code(&self) -> &'static str {
        match self {
            Who::Nobody(_) => "nobody",
            Who::Local(_) => "local",
            Who::Person(_) => "person",
        }
    }

    /// Who signs records, and whose shelf is open.
    pub fn me(&self) -> Result<&Me, &'static str> {
        match self {
            Who::Nobody(code) => Err(*code),
            Who::Local(me) | Who::Person(me) => Ok(me),
        }
    }

    /// Who may ask our server, host a room, or file to a city.
    pub fn person(&self) -> Option<&Me> {
        match self {
            Who::Person(me) => Some(me),
            Who::Nobody(_) | Who::Local(_) => None,
        }
    }
}

/// The rotation key of an identity made here, kept until the person says they saved it
/// elsewhere. did.md asks for that before going on; the owner chose to remind them later.
const UNSAVED: &str = "rotation.unsaved";

/// How long the directory has to take a new DID. Origin to be traced.
pub(crate) const CREATE_WAIT_MS: u64 = 30_000;
/// How often a sign-in asks the relay whether the other device has sent the key. Origin to
/// be traced.
pub(crate) const SIGN_IN_POLL_MS: u64 = 1_000;
/// How long a sign-in code stays good: five minutes to scan it. Origin to be traced.
pub(crate) const SIGN_IN_WAIT_MS: u64 = 300_000;
/// The issuer says a connection's cap in seconds (`session_cap_s`); time here is in ms.
const MS_PER_S: u64 = 1_000;

/// A Zu identity made on this device for someone who has none (did-zu.md § DID Creation): a
/// signing key, and a rotation key that is kept here too. Nothing is kept unless the
/// directory at `directory` took the DID.
pub async fn create(http: &Client, directory: &str) -> Result<Me, End> {
    let (seed, secret) = (Zeroizing::new(random::<32>()?), Zeroizing::new(random::<32>()?));
    // About one draw in 2^32 is no P-256 key: the next attempt draws again.
    let rotation = did::Rotation::from_bytes(&secret).map_err(|_| End::Retry("keys.random"))?;
    let made = did::create(&rotation, &SecretKey::from_seed(&seed).public());
    let (did, op) = made.map_err(|_| End::Fatal("id.sign"))?;
    let post = http.post(format!("{directory}/{did}")).header(CONTENT_TYPE, "application/json");
    let sent = post.body(op).timeout(Duration::from_millis(CREATE_WAIT_MS)).send().await;
    match sent.map_err(|_| End::Retry("offline"))?.status() {
        status if status.is_success() => {}
        status if status.is_server_error() => return Err(End::Retry("server")),
        _ => return Err(End::Fatal("id.directory")), // it refused the operation
    }
    set(UNSAVED, Some(secret.as_slice()))?;
    keep(&seed, did)
}

/// The rotation key of the identity made here, written to be saved, whether or not the person
/// said they saved it already. `None`: this device made no identity.
pub fn rotation() -> Result<Option<Zeroizing<String>>, End> {
    let Some(secret) = (match get(UNSAVED)? {
        None => get("rotation")?,
        unsaved => unsaved,
    }) else {
        return Ok(None);
    };
    let secret: &[u8; 32] = secret.as_slice().try_into().map_err(|_| End::Fatal("keys.store"))?;
    let rotation = did::Rotation::from_bytes(secret).map_err(|_| End::Fatal("keys.store"))?;
    Ok(Some(rotation.multikey()))
}

/// Whether the person is still to be reminded to save the rotation key made here.
pub fn unsaved() -> bool {
    get(UNSAVED).is_ok_and(|key| key.is_some())
}

/// The person saved the rotation key made here: it is kept still, and they are not reminded.
pub fn saved() -> Result<(), End> {
    if let Some(key) = get(UNSAVED)? {
        set("rotation", Some(&key))?;
        set(UNSAVED, None)?;
    }
    Ok(())
}

/// The access token of a community invite the 0.8 app left at `path`, as
/// `{"version":1,"accessToken":"…"}`, unless it was carried over already. Only that file is
/// read, and it is left as it is.
pub fn invite(path: &Path) -> Result<Option<Secret>, End> {
    let json = match fs::read(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        read => Zeroizing::new(read.map_err(|_| End::Fatal("invite.file"))?),
    };
    let json: Json = serde_json::from_slice(&json).map_err(|_| End::Fatal("invite.file"))?;
    let token = json["accessToken"].as_str().map(str::trim).filter(|token| !token.is_empty());
    let token = token.filter(|_| json["version"] == 1).ok_or(End::Fatal("invite.file"))?;
    let token = Secret::new(token);
    Ok((get("invite.carried")?.as_deref() != Some(&mark(&token))).then_some(token))
}

/// The invite went over, or was refused for good: it is not sent again.
pub fn carried(token: &Secret) -> Result<(), End> {
    set("invite.carried", Some(&mark(token)))
}

fn mark(token: &Secret) -> Vec<u8> {
    Sha256::digest(token.expose()).to_vec()
}

/// A sign-in under way: the code to show, how to open what comes back, and the relay URL to poll.
pub struct SignIn {
    code: login::Code,
    relay_room: String,
}

impl SignIn {
    /// `origin` relays the key: our server. Loopback `http://127.0.0.1:` / `http://localhost:`
    /// origins used by local tests wrap as `https://` inside the `zu::login` code while polling
    /// the loopback relay over plain HTTP; every other origin must already be `https://`.
    pub fn begin(origin: &str) -> Result<SignIn, End> {
        let (secret, room) = (Zeroizing::new(random::<32>()?), random::<16>()?);
        let local_http =
            origin.starts_with("http://127.0.0.1:") || origin.starts_with("http://localhost:");
        let code_origin = if local_http {
            format!("https://{}", &origin["http://".len()..])
        } else {
            origin.to_owned()
        };
        let code =
            login::Code::new(&code_origin, &secret, &room).map_err(|_| End::Fatal("id.code"))?;
        let relay_room = if local_http {
            code.room.replacen("https://", "http://", 1)
        } else {
            code.room.clone()
        };
        Ok(SignIn { code, relay_room })
    }

    /// What to show as a QR code for the other device to scan.
    pub fn code(&self) -> &str {
        &self.code.url
    }

    pub(crate) fn login_code(&self) -> &login::Code {
        &self.code
    }

    pub(crate) fn relay_room(&self) -> &str {
        &self.relay_room
    }

    /// Waits for the key, checks it is a current key of its DID, and keeps it.
    pub async fn finish(&self, http: &Client) -> Result<Me, End> {
        let (deadline, offline) = (wall_ms() + SIGN_IN_WAIT_MS, |_| End::Retry("offline"));
        let sealed = loop {
            let reply = http.get(&self.relay_room).send().await.map_err(offline)?;
            match reply.status() {
                StatusCode::OK => break reply.bytes().await.map_err(offline)?,
                StatusCode::NOT_FOUND if wall_ms() < deadline => {}
                _ => return Err(End::Fatal("id.expired")),
            }
            tokio::time::sleep(Duration::from_millis(SIGN_IN_POLL_MS)).await;
        };
        let received = self.code.open(&sealed, None).map_err(|_| End::Fatal("id.sealed"))?;
        let url = did::url(&received.did, false).map_err(|_| End::Fatal("id.did"))?;
        let doc = http.get(url).send().await.and_then(|r| r.error_for_status()).map_err(offline)?;
        let doc = did::resolve(&received.did, &doc.bytes().await.map_err(offline)?);
        let seed = received.seed(&doc.map_err(|_| End::Fatal("id.did"))?);
        keep(seed.map_err(|_| End::Fatal("id.key"))?, received.did.clone())
    }
}

/// Our server, to the person signed in here. Every request is a record they sign, made for
/// this server and made now; the server keeps what it needs as its proof.
#[derive(Clone)]
pub struct Ours {
    pub server: String,
    pub me: Me,
    pub http: Client,
}

/// The server's refusals that mean something here, by their stable code; the rest is trouble
/// that may pass, or a person the server does not take as who they say.
const REFUSED: [(&str, &str); 8] = [
    ("quota", "keys.quota"),
    ("quota.daily", "keys.quota.daily"),
    ("claimed", "invite.claimed"),
    ("invite", "invite.unknown"),
    ("room", "room.gone"),
    ("owner", "room.owner"),
    ("full", "room.full"),
    ("rooms", "room.rooms"),
];

impl Ours {
    /// Sends `body`, with this server as its `aud`, as a fresh record of type `ty`.
    pub async fn ask(&self, route: &str, ty: &str, mut body: Json) -> Result<Json, End> {
        let failed = |_| End::Fatal("id.sign");
        body["aud"] = json!(self.server);
        let value = Value::from_json(&body.to_string()).map_err(failed)?;
        let path = zu::Path::new(&self.me.did, ty, &fresh()?);
        let (cid, at) = (Cid::of(Codec::Drisl, &value.encode()), wall_ms() as i64);
        let record = Record::sign(&self.me.key, path.map_err(failed)?, Some(cid), [], at, None);
        let header = record.map_err(failed)?.header().to_json().map_err(failed)?;
        let header: Json = serde_json::from_str(&header).map_err(|_| End::Fatal("id.sign"))?;
        let sent = json!({ "record": header, "body": body });
        let reply = self.http.post(format!("{}/{route}", self.server)).json(&sent).send().await;
        let reply = reply.map_err(|_| End::Retry("offline"))?;
        let status = reply.status();
        let said: Json = reply.json().await.unwrap_or_default();
        if status.is_success() {
            return Ok(said);
        }
        let refused = REFUSED.iter().find(|(theirs, _)| said["error"] == *theirs);
        Err(match refused {
            Some((_, ours)) => End::Fatal(ours),
            None if [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN].contains(&status) => {
                End::Fatal("id.refused")
            }
            None => End::Retry("server"),
        })
    }

    /// A short-lived key drawn from granted quota, what is left of it, and any updated daily quota.
    async fn issue(&self, service: Service) -> Result<(Lease, u64, Option<Quota>), End> {
        let issued = self.ask("lease", "talk/lease", json!({ "service": service.name() })).await?;
        let key = Secret::new(issued["key"].as_str().ok_or(End::Retry("server"))?);
        let session_cap = issued["session_cap_s"].as_u64().map(|cap_s| {
            let cap_ms = cap_s.saturating_mul(MS_PER_S);
            Duration::from_millis(cap_ms)
        });
        let quota = issued.get("quota").and_then(quota::parse_quota);
        Ok((Lease { key, session_cap }, issued["left"].as_u64().unwrap_or(0), quota))
    }
}

/// Where a service's key comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
pub enum From {
    #[default]
    Nowhere,
    Own,
    Granted,
}

impl From {
    /// Own key wins over a granted lease; with neither there is nowhere to get one.
    pub fn of(own: bool, person: bool) -> From {
        if own {
            From::Own
        } else if person {
            From::Granted
        } else {
            From::Nowhere
        }
    }
}

/// The person's own key for a service if they set one, else the issuer's. A key the service
/// refused is not handed out again until it is replaced.
pub struct Chain {
    refused: Mutex<Vec<Service>>,
    issuer: Option<Ours>,
    left: AtomicU64, // µ$ of granted quota, as the issuer last said; `MAX`: not heard yet
    quota: Mutex<Option<Quota>>,
}

impl Chain {
    pub fn new(issuer: Option<Ours>) -> Self {
        Self {
            refused: Mutex::default(),
            issuer,
            left: AtomicU64::new(u64::MAX),
            quota: Mutex::new(None),
        }
    }

    /// The person set or cleared their own key: whatever was refused before is forgotten.
    pub fn own(&self, service: Service, key: Option<&str>) -> Result<(), End> {
        set(service.name(), key.map(str::as_bytes))?;
        self.refused.lock().iter_mut().for_each(|refused| refused.retain(|s| *s != service));
        Ok(())
    }

    pub fn has_own(&self, service: Service) -> bool {
        get(service.name()).is_ok_and(|key| key.is_some())
    }

    /// Where a key for `service` would come from right now.
    pub fn from(&self, service: Service) -> From {
        From::of(self.has_own(service), self.issuer.is_some())
    }

    pub fn left(&self) -> Option<u64> {
        Some(self.left.load(Relaxed)).filter(|left| *left != u64::MAX)
    }

    pub fn quota(&self) -> Option<Quota> {
        self.quota.lock().ok().and_then(|q| q.clone())
    }

    pub fn set_quota(&self, left: u64, quota: Quota) {
        self.left.store(left, Relaxed);
        if let Ok(mut guard) = self.quota.lock() {
            *guard = Some(quota);
        }
    }

    /// Carries the hours left on a 0.8 community invite into what is granted here. The token
    /// goes to our server, which has the old service retire it, and is kept nowhere.
    pub async fn claim(&self, token: &Secret) -> Result<(), End> {
        let issuer = self.issuer.as_ref().ok_or(End::Fatal("sign_in.first"))?;
        let said = issuer.ask("claim", "talk/claim", json!({ "token": token.expose() })).await?;
        self.left.store(said["left"].as_u64().unwrap_or(0), Relaxed);
        if let Some(q) = said.get("quota").and_then(quota::parse_quota)
            && let Ok(mut guard) = self.quota.lock()
        {
            *guard = Some(q);
        }
        Ok(())
    }
}

impl Keys for Chain {
    async fn lease(&self, service: Service) -> Result<Lease, End> {
        let refused = self.refused.lock().is_ok_and(|refused| refused.contains(&service));
        if let (false, Some(key)) = (refused, get(service.name())?) {
            let key = String::from_utf8(key.to_vec()).map_err(|_| End::Fatal("keys.store"))?;
            return Ok(Lease { key: Secret(key), session_cap: None });
        }
        let issuer = self.issuer.as_ref().ok_or(End::Fatal(if refused {
            "keys.refused"
        } else {
            "keys.none"
        }))?;
        let (lease, left, quota) = issuer.issue(service).await?;
        self.left.store(left, Relaxed);
        if let Some(q) = quota
            && let Ok(mut guard) = self.quota.lock()
        {
            *guard = Some(q);
        }
        Ok(lease)
    }

    fn refused(&self, service: Service) {
        self.refused.lock().iter_mut().for_each(|refused| refused.push(service));
    }
}
