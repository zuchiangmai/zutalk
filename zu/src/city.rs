//! city.md: who is a resident of a city and who holds which role, worked out from the city's
//! governance records. Everyone who holds the same chain reaches the same state. The records
//! here are already verified (record.md § Verifying a Record); which ones to hold is the
//! caller's. This follows pol's `packages/zu/src/city.ts`, and both pass pol's
//! `vectors/city.json`.

use crate::{Cid, Record, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// § The Records: the type of the governance records.
pub const ROLE_TYPE: &str = "city/role";
/// § Invitations: the type of an invitation.
pub const INVITE_TYPE: &str = "city/invite";
/// § Rules, 5: a proposal closes this long after its `at`, in ms (14 days).
pub const PROPOSAL_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// § Roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Coordinator,
    Curator,
    Inviter,
}

/// § The Records: what a `city/role` record is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Resident,
    Role(Role),
}

/// The `kind` of a body as written, and what it is. A kind not here is one software ignores.
const KINDS: [(&str, Kind); 4] = [
    ("resident", Kind::Resident),
    ("coordinator", Kind::Role(Role::Coordinator)),
    ("curator", Kind::Role(Role::Curator)),
    ("inviter", Kind::Role(Role::Inviter)),
];

impl Role {
    pub const ALL: [Role; 3] = [Role::Coordinator, Role::Curator, Role::Inviter];

    /// The `kind` that names it.
    pub fn name(self) -> &'static str {
        KINDS.iter().find(|(_, kind)| *kind == Kind::Role(self)).map_or("", |(name, _)| name)
    }
}

/// § The Records: the body of a `city/role` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleBody {
    pub who: String,
    pub kind: Kind,
    pub ok: bool,
    /// In a join only: the secret of the invitation it uses.
    pub code: Option<Vec<u8>>,
}

impl RoleBody {
    /// The body, or `None` when software ignores it: not a map, a field missing or of the wrong
    /// type, or a `kind` it does not know.
    pub fn read(body: &Value) -> Option<RoleBody> {
        let Value::Map(map) = body else { return None };
        let (Some(Value::Str(who)), Some(Value::Str(kind)), Some(Value::Bool(ok))) =
            (map.get("who"), map.get("kind"), map.get("ok"))
        else {
            return None;
        };
        let kind = KINDS.iter().find(|(name, _)| name == kind)?.1;
        let code = match map.get("code") {
            None => None,
            Some(Value::Bytes(code)) => Some(code.clone()),
            Some(_) => return None,
        };
        Some(RoleBody { who: who.clone(), kind, ok: *ok, code })
    }
}

/// One governance record: its CID, its header, and its body, `None` when the body did not come
/// or software ignores it. Such a record stays on the chain, with no effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Governance {
    pub cid: Cid,
    pub record: Record,
    pub body: Option<RoleBody>,
}

impl Governance {
    pub fn new(record: Record, body: Option<&Value>) -> Governance {
        Governance { cid: record.cid(), body: body.and_then(RoleBody::read), record }
    }
}

/// § Invitations: an invitation current at its path, and the hash of its secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    pub cid: Cid,
    pub by: String,
    pub to: Option<String>,
    pub hash: [u8; 32],
}

impl Invitation {
    /// The invitation, or `None` for a body without a `hash` of 32 bytes.
    pub fn new(record: &Record, body: &Value) -> Option<Invitation> {
        let Value::Map(map) = body else { return None };
        let Some(Value::Bytes(hash)) = map.get("hash") else { return None };
        let hash = <[u8; 32]>::try_from(hash.as_slice()).ok()?;
        Some(Invitation {
            cid: record.cid(),
            by: record.path.by.clone(),
            to: record.to.clone(),
            hash,
        })
    }
}

/// § The Chain: the governance records in chain order, and those a host erred with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chain {
    pub chain: Vec<Governance>,
    /// Records that could have been next where another was: two records listing the same one.
    pub forks: Vec<Governance>,
}

/// § The Chain, § Working Out the State: orders a city's governance records. Each record lists
/// the one before it in `prev`, besides records it acts on, so the next record is one that
/// lists the last and whose other governance records are all placed. Of several, the earliest
/// `at` is next, then the lowest CID; the others are forks. A record that lists only a fork is
/// on neither list. The same CID given twice counts once.
pub fn order(records: Vec<Governance>) -> Chain {
    let mut seen = BTreeSet::new();
    let records: Vec<Governance> = records.into_iter().filter(|r| seen.insert(r.cid)).collect();
    let index: HashMap<Cid, usize> = records.iter().enumerate().map(|(i, r)| (r.cid, i)).collect();
    // The governance records each lists, by index; those it lists that are not, do not count.
    let before: Vec<Vec<usize>> = records
        .iter()
        .map(|r| r.record.prev.iter().filter_map(|cid| index.get(cid).copied()).collect())
        .collect();
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); records.len()];
    for (i, listed) in before.iter().enumerate() {
        listed.iter().for_each(|&j| after[j].push(i));
    }
    let mut placed = vec![false; records.len()];
    let (mut chain, mut forks) = (Vec::new(), Vec::new());
    let mut next: Vec<usize> = (0..records.len()).filter(|&i| before[i].is_empty()).collect();
    while !next.is_empty() {
        next.sort_by_key(|&i| (records[i].record.at, records[i].cid));
        next.iter().for_each(|&i| placed[i] = true);
        let last = next[0];
        chain.push(last);
        forks.extend_from_slice(&next[1..]);
        next = after[last]
            .iter()
            .copied()
            .filter(|&i| !placed[i] && before[i].iter().all(|&j| placed[j]))
            .collect();
    }
    let mut slots: Vec<Option<Governance>> = records.into_iter().map(Some).collect();
    let mut take = |at: Vec<usize>| -> Vec<Governance> {
        at.into_iter().filter_map(|i| slots[i].take()).collect()
    };
    Chain { chain: take(chain), forks: take(forks) }
}

/// § Rules, 5: a proposal that has not passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    /// The vote that opened it.
    pub opened: Cid,
    pub who: String,
    pub ok: bool,
    pub at: i64,
    /// The coordinators in office just before it opened.
    pub n: usize,
    pub votes: BTreeSet<String>,
}

/// A city's state, worked out one governance record at a time by the six rules, in chain order
/// (§ Rules, 1: each record is judged by the state the ones before it produced).
#[derive(Clone, Debug)]
pub struct Fold {
    city: String,
    pub residents: BTreeSet<String>,
    /// The roles of each person; a person whose last role ended may stay with none.
    pub roles: BTreeMap<String, BTreeSet<Role>>,
    /// Open by `who` and direction.
    proposals: BTreeMap<(String, bool), Proposal>,
    /// The joins on the chain so far: CID, whose.
    joins: HashMap<Cid, String>,
    /// Invitations used already.
    used: BTreeSet<Cid>,
}

impl Fold {
    pub fn new(city: &str) -> Fold {
        Fold {
            city: city.to_owned(),
            residents: BTreeSet::new(),
            roles: BTreeMap::new(),
            proposals: BTreeMap::new(),
            joins: HashMap::new(),
            used: BTreeSet::new(),
        }
    }

    pub fn holds(&self, did: &str, role: Role) -> bool {
        self.roles.get(did).is_some_and(|held| held.contains(&role))
    }

    /// The proposals that have not passed. Whether one is still open depends on when it is asked.
    pub fn proposals(&self) -> impl Iterator<Item = &Proposal> {
        self.proposals.values()
    }

    /// Applies the next record of the chain. `invitations` are those current now; one counts
    /// once, for the first join that carries its secret.
    pub fn apply(&mut self, step: &Governance, invitations: &[Invitation]) {
        let (record, Some(body)) = (&step.record, &step.body) else { return };
        if record.to.as_deref() != Some(self.city.as_str()) {
            return;
        }
        let by = record.path.by.as_str();
        let lists_join = |fold: &Fold| {
            record.prev.iter().any(|cid| fold.joins.get(cid).is_some_and(|who| *who == body.who))
        };

        // Rule 2: the city acts at once.
        if by == self.city {
            match body.kind {
                Kind::Resident if body.ok && lists_join(self) => {
                    self.residents.insert(body.who.clone());
                }
                Kind::Resident => {}
                Kind::Role(role) if body.ok => self.grant(&body.who, role),
                Kind::Role(role) => self.revoke(&body.who, role),
            }
            return;
        }

        // Rule 3: residents join with an admission, and leave on their own.
        if body.kind == Kind::Resident {
            if by == body.who && body.ok {
                self.joins.insert(step.cid, by.to_owned());
                let invitation = body
                    .code
                    .as_deref()
                    .and_then(|code| self.invitation(code, record, invitations));
                if let Some(cid) = invitation {
                    self.used.insert(cid);
                    self.residents.insert(by.to_owned());
                }
            } else if by == body.who {
                self.residents.remove(by);
                self.roles.remove(by);
            } else if body.ok && self.holds(by, Role::Coordinator) && lists_join(self) {
                self.residents.insert(body.who.clone());
            }
            return;
        }
        let Kind::Role(role) = body.kind else { return };

        // Rule 6: anyone gives up a role they hold.
        if by == body.who {
            if !body.ok {
                self.revoke(by, role);
            }
            return;
        }
        if !self.holds(by, Role::Coordinator) {
            return;
        }

        // Rule 4: a coordinator appoints and dismisses curators and inviters at once.
        if role != Role::Coordinator {
            if body.ok {
                self.grant(&body.who, role)
            } else {
                self.revoke(&body.who, role)
            }
            return;
        }

        // Rule 5: coordinators vote on another's coordinator role; n / 2 + 1 of those in office
        // just before the proposal opened.
        let key = (body.who.clone(), body.ok);
        let closed = self.proposals.get(&key).is_some_and(|p| record.at > p.at + PROPOSAL_MS);
        if closed {
            self.proposals.remove(&key);
        }
        let n = self.coordinators();
        let proposal = self.proposals.entry(key.clone()).or_insert_with(|| Proposal {
            opened: step.cid,
            who: body.who.clone(),
            ok: body.ok,
            at: record.at,
            n,
            votes: BTreeSet::new(),
        });
        if !proposal.votes.insert(by.to_owned()) {
            return;
        }
        if proposal.votes.len() > proposal.n / 2 {
            self.proposals.remove(&key);
            if body.ok {
                self.grant(&body.who, Role::Coordinator)
            } else {
                self.revoke(&body.who, Role::Coordinator)
            }
        }
    }

    /// § Rules, 3: only a resident holds a role.
    fn grant(&mut self, did: &str, role: Role) {
        if self.residents.contains(did) {
            self.roles.entry(did.to_owned()).or_default().insert(role);
        }
    }

    fn revoke(&mut self, did: &str, role: Role) {
        if let Some(held) = self.roles.get_mut(did) {
            held.remove(&role);
        }
    }

    fn coordinators(&self) -> usize {
        self.roles.values().filter(|held| held.contains(&Role::Coordinator)).count()
    }

    /// § Invitations: the unused invitation a join lists in `prev` and carries the secret of,
    /// opened for this city by an inviter or a coordinator in office.
    fn invitation(&self, code: &[u8], join: &Record, invitations: &[Invitation]) -> Option<Cid> {
        let hash: [u8; 32] = Sha256::digest(code).into();
        let found = invitations.iter().find(|inv| {
            join.prev.contains(&inv.cid)
                && !self.used.contains(&inv.cid)
                && inv.to.as_deref() == Some(self.city.as_str())
                && inv.hash == hash
                && (self.holds(&inv.by, Role::Inviter) || self.holds(&inv.by, Role::Coordinator))
        });
        found.map(|inv| inv.cid)
    }
}

/// § Working Out the State: the rules applied from the first record of the chain to the last.
pub fn state(city: &str, chain: &[Governance], invitations: &[Invitation]) -> Fold {
    let mut fold = Fold::new(city);
    chain.iter().for_each(|step| fold.apply(step, invitations));
    fold
}

/// One spell of someone in an office: from the `at` of the record that gave it to the `at` of
/// the one that took it away, `None` while it is held.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Term {
    pub from: i64,
    pub who: String,
    pub role: Role,
    pub until: Option<i64>,
}

/// Who held which office when: the chain applied one record at a time, each change noted at the
/// `at` of the record that made it. Each record of a chain lists the one before it, and a
/// record's `at` is after those it lists (record.md), so the records signed at or before a time
/// are the first ones of the chain; were one earlier than a record before it, its change is
/// noted at the latest `at` so far. Ordered by `from`.
pub fn terms(city: &str, chain: &[Governance], invitations: &[Invitation]) -> Vec<Term> {
    let mut fold = Fold::new(city);
    let (mut done, mut open) = (Vec::new(), BTreeMap::<(String, Role), i64>::new());
    let mut at = i64::MIN;
    for step in chain {
        fold.apply(step, invitations);
        at = at.max(step.record.at);
        let ended: Vec<(String, Role)> =
            open.keys().filter(|(who, role)| !fold.holds(who, *role)).cloned().collect();
        for (who, role) in ended {
            let from = open.remove(&(who.clone(), role)).unwrap_or(at);
            done.push(Term { from, who, role, until: Some(at) });
        }
        for (who, held) in &fold.roles {
            for &role in held {
                open.entry((who.clone(), role)).or_insert(at);
            }
        }
    }
    let held = open.into_iter().map(|((who, role), from)| Term { from, who, role, until: None });
    done.extend(held);
    done.sort();
    done
}

/// Whether `did` held `role` once the chain's records signed at or before `at` were applied.
pub fn held_at(terms: &[Term], did: &str, role: Role, at: i64) -> bool {
    terms.iter().any(|t| {
        t.who == did && t.role == role && t.from <= at && t.until.is_none_or(|until| at < until)
    })
}
