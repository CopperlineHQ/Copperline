// SPDX-License-Identifier: GPL-3.0-or-later

//! Deterministic device-level fault injection for image-backed block
//! devices (`docs/debugger/disk-fault.md`): dropped/torn/reordered
//! writes, ignored flushes, stale/corrupt reads, media errors, delays
//! and simulated power-cuts. This type (`FaultPolicySet`) is the one
//! engine every target kind installs: on
//! [`crate::harddrive::HardDriveImage`] itself for copperhf/IDE/SCSI/
//! PCMCIA (every controller that already shares that sector-I/O layer
//! gets it for free -- see `HardDriveImage::read_sector`/`write_sector`/
//! `flush`), on `FloppyImage` (`src/floppy/formats.rs`, `StandardAdf`
//! images only -- extended ADF/raw MFM has no sector grid to fault, see
//! `FloppyImage::has_sector_model`'s doc comment) for `floppy`, and on
//! [`crate::cdrom::CdImage`] (read-side actions only -- CD is read-only
//! medium) for `cd`.
//!
//! Every random choice (probability tests, reorder release order, corrupt
//! bit positions) is drawn from a generator keyed on the policy's seed and
//! the target's own request sequence number, never on wall-clock time or
//! host state, so two runs with the same image, guest input schedule and
//! seed fault identically -- the control-protocol proposal's
//! "Determinism" requirement.

use std::collections::VecDeque;

pub const MAX_POLICIES_PER_TARGET: usize = 16;
pub const MAX_RULES_PER_POLICY: usize = 64;
pub const MAX_REORDER_WINDOW: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Op {
    Read,
    Write,
    Flush,
    Trim,
    Any,
}

impl Op {
    fn matches(self, actual: Op) -> bool {
        self == Op::Any || self == actual
    }

    fn parse(s: &str) -> Result<Op, String> {
        match s {
            "read" => Ok(Op::Read),
            "write" => Ok(Op::Write),
            "flush" => Ok(Op::Flush),
            "trim" => Ok(Op::Trim),
            "any" => Ok(Op::Any),
            other => Err(format!("unknown op {other:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LbaRange {
    pub from: u64,
    pub to: u64,
}

impl LbaRange {
    /// Matches if any sector of a `[lba, lba + count)` request falls in
    /// the (inclusive) range.
    fn overlaps(&self, lba: u64, count: u64) -> bool {
        let end = lba.saturating_add(count.max(1)) - 1;
        lba <= self.to && end >= self.from
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AfterKind {
    Requests,
    Reads,
    Writes,
    Flushes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct After {
    pub kind: AfterKind,
    pub n: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TearAt {
    Sector,
    Bytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OnClear {
    Release,
    Drop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IgnoreThen {
    Hold,
    Drop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ErrorCode {
    Media,
    Timeout,
    NotReady,
    WriteProtect,
    Aborted,
}

impl ErrorCode {
    fn parse(s: &str) -> Result<ErrorCode, String> {
        match s {
            "media" => Ok(ErrorCode::Media),
            "timeout" => Ok(ErrorCode::Timeout),
            "not_ready" => Ok(ErrorCode::NotReady),
            "write_protect" => Ok(ErrorCode::WriteProtect),
            "aborted" => Ok(ErrorCode::Aborted),
            other => Err(format!("unknown error code {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Media => "media",
            ErrorCode::Timeout => "timeout",
            ErrorCode::NotReady => "not_ready",
            ErrorCode::WriteProtect => "write_protect",
            ErrorCode::Aborted => "aborted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Action {
    Drop,
    Tear { at: TearAt, n: u64 },
    Reorder { window: usize },
    Ignore { then: IgnoreThen },
    Stale { count: u64 },
    Corrupt { bits: u32 },
    Error { code: ErrorCode },
    Delay { ms: u64 },
    Cut,
}

impl Action {
    fn name(&self) -> &'static str {
        match self {
            Action::Drop => "drop",
            Action::Tear { .. } => "tear",
            Action::Reorder { .. } => "reorder",
            Action::Ignore { .. } => "ignore",
            Action::Stale { .. } => "stale",
            Action::Corrupt { .. } => "corrupt",
            Action::Error { .. } => "error",
            Action::Delay { .. } => "delay",
            Action::Cut => "cut",
        }
    }
}

/// A rule's per-installation runtime state: everything that changes as
/// requests are matched against it. Kept separate from the rule's static
/// config so `Rule` stays trivially comparable/constructable from parsed
/// JSON, and so `state.save`/`state.load` (`FaultPolicySet`'s own
/// `Serialize`/`Deserialize`) captures exactly the fields the proposal
/// requires: "held-write queues, rule counters and generator state".
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RuleState {
    pub applied: u64,
    pub hits: u64,
    pub reorder_queue: VecDeque<HeldWrite>,
    /// `stale`: lba -> (reads remaining at the old value, the pre-write
    /// bytes to keep serving).
    pub stale_pending: std::collections::HashMap<u64, (u64, Vec<u8>)>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HeldWrite {
    pub lba: u64,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Rule {
    pub op: Op,
    pub lba: Option<LbaRange>,
    pub probability: f64,
    pub after: Option<After>,
    pub times: Option<u64>,
    pub keep_going: bool,
    pub action: Action,
    #[serde(default)]
    pub state: RuleState,
}

impl Rule {
    fn from_json(v: &serde_json::Value, index: usize) -> Result<Rule, String> {
        let err = |field: &str, msg: &str| format!("rule {index}: {field}: {msg}");
        let obj = v
            .as_object()
            .ok_or_else(|| format!("rule {index}: not an object"))?;
        let op = match obj.get("op") {
            Some(s) => Op::parse(s.as_str().ok_or_else(|| err("op", "not a string"))?)
                .map_err(|e| err("op", &e))?,
            None => Op::Any,
        };
        let lba = match obj.get("lba") {
            Some(v) => {
                let from = v
                    .get("from")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| err("lba", "missing from"))?;
                let to = v
                    .get("to")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| err("lba", "missing to"))?;
                Some(LbaRange { from, to })
            }
            None => None,
        };
        let probability = match obj.get("probability") {
            Some(v) => v
                .as_f64()
                .filter(|p| (0.0..=1.0).contains(p))
                .ok_or_else(|| err("probability", "not a number in 0.0..=1.0"))?,
            None => 1.0,
        };
        let after = match obj.get("after") {
            Some(v) => {
                let (kind, n) = ["requests", "reads", "writes", "flushes"]
                    .iter()
                    .find_map(|k| {
                        v.get(k)
                            .and_then(serde_json::Value::as_u64)
                            .map(|n| (*k, n))
                    })
                    .ok_or_else(|| err("after", "missing requests/reads/writes/flushes"))?;
                let kind = match kind {
                    "requests" => AfterKind::Requests,
                    "reads" => AfterKind::Reads,
                    "writes" => AfterKind::Writes,
                    _ => AfterKind::Flushes,
                };
                Some(After { kind, n })
            }
            None => None,
        };
        let times = obj.get("times").and_then(serde_json::Value::as_u64);
        let keep_going = obj
            .get("continue")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let action_name = obj
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| err("action", "missing"))?;
        let action = match action_name {
            "drop" => Action::Drop,
            "tear" => {
                let at = match obj.get("at").and_then(serde_json::Value::as_str) {
                    Some("bytes") => TearAt::Bytes,
                    Some("sector") | None => TearAt::Sector,
                    Some(other) => return Err(err("at", &format!("unknown {other:?}"))),
                };
                let n = obj
                    .get("n")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                Action::Tear { at, n }
            }
            "reorder" => {
                let window = obj
                    .get("window")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(1) as usize;
                if window == 0 || window > MAX_REORDER_WINDOW {
                    return Err(err("window", "must be 1..=64"));
                }
                Action::Reorder { window }
            }
            "ignore" => {
                let then = match obj.get("then").and_then(serde_json::Value::as_str) {
                    Some("drop") => IgnoreThen::Drop,
                    Some("hold") | None => IgnoreThen::Hold,
                    Some(other) => return Err(err("then", &format!("unknown {other:?}"))),
                };
                Action::Ignore { then }
            }
            "stale" => {
                let count = obj
                    .get("count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(1);
                Action::Stale { count }
            }
            "corrupt" => {
                let bits = obj
                    .get("bits")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(1) as u32;
                Action::Corrupt { bits }
            }
            "error" => {
                let code = obj
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| err("code", "missing"))?;
                Action::Error {
                    code: ErrorCode::parse(code).map_err(|e| err("code", &e))?,
                }
            }
            "delay" => {
                let ms = obj
                    .get("ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                Action::Delay { ms }
            }
            "cut" => Action::Cut,
            other => return Err(err("action", &format!("unknown {other:?}"))),
        };
        Ok(Rule {
            op,
            lba,
            probability,
            after,
            times,
            keep_going,
            action,
            state: RuleState::default(),
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "op".into(),
            serde_json::Value::String(
                match self.op {
                    Op::Read => "read",
                    Op::Write => "write",
                    Op::Flush => "flush",
                    Op::Trim => "trim",
                    Op::Any => "any",
                }
                .into(),
            ),
        );
        if let Some(lba) = self.lba {
            obj.insert(
                "lba".into(),
                serde_json::json!({"from": lba.from, "to": lba.to}),
            );
        }
        obj.insert("probability".into(), serde_json::json!(self.probability));
        obj.insert(
            "action".into(),
            serde_json::Value::String(self.action.name().into()),
        );
        match &self.action {
            Action::Tear { at, n } => {
                obj.insert(
                    "at".into(),
                    serde_json::Value::String(
                        match at {
                            TearAt::Sector => "sector",
                            TearAt::Bytes => "bytes",
                        }
                        .into(),
                    ),
                );
                obj.insert("n".into(), serde_json::json!(n));
            }
            Action::Reorder { window } => {
                obj.insert("window".into(), serde_json::json!(window));
            }
            Action::Ignore { then } => {
                obj.insert(
                    "then".into(),
                    serde_json::Value::String(
                        match then {
                            IgnoreThen::Hold => "hold",
                            IgnoreThen::Drop => "drop",
                        }
                        .into(),
                    ),
                );
            }
            Action::Stale { count } => {
                obj.insert("count".into(), serde_json::json!(count));
            }
            Action::Corrupt { bits } => {
                obj.insert("bits".into(), serde_json::json!(bits));
            }
            Action::Error { code } => {
                obj.insert(
                    "code".into(),
                    serde_json::Value::String(code.as_str().into()),
                );
            }
            Action::Delay { ms } => {
                obj.insert("ms".into(), serde_json::json!(ms));
            }
            Action::Drop | Action::Cut => {}
        }
        obj.insert("hits".into(), serde_json::json!(self.state.hits));
        serde_json::Value::Object(obj)
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub requests: u64,
    pub reads: u64,
    pub writes: u64,
    pub flushes: u64,
    pub trims: u64,
    pub dropped: u64,
    pub torn: u64,
    pub reordered: u64,
    pub flush_ignored: u64,
    pub stale_served: u64,
    pub corrupted: u64,
    pub errored: u64,
    pub delayed_ms: u64,
    pub cut: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Policy {
    pub id: String,
    pub seed: u64,
    pub on_clear: OnClear,
    pub rules: Vec<Rule>,
}

/// One target's whole fault-injection state: its installed policies (in
/// installation order -- `set` with an `id` replaces one in place, keeping
/// its position), the shared request-sequence counter every policy's
/// generator is keyed on, and cumulative stats. Lives on
/// [`crate::harddrive::HardDriveImage`] directly (`fault` field) so it
/// travels with the image and needs no separate target registry.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FaultPolicySet {
    pub policies: Vec<Policy>,
    pub seq: u64,
    pub stats: Stats,
    /// Drained by the control layer for `events.subscribe`'s `disk.fault`
    /// family -- not state (never saved/loaded), so it is skipped rather
    /// than round-tripped through a save state the way `policies`/`seq`
    /// are.
    #[serde(skip)]
    pub pending_events: Vec<FaultEvent>,
    /// Set by `cut`: every request fails `not_ready` until an explicit
    /// `disk.fault.clear`, even across a `machine.reset` (the proposal's
    /// "power-cut recovery test" -- a reset must not quietly un-cut the
    /// drive).
    pub cut: bool,
}

/// What a caller (`HardDriveImage::read_sector` and friends) must do with
/// one request after the policy set has had a chance to act on it.
pub enum Outcome {
    /// Proceed with the real I/O unmodified.
    Proceed,
    /// The request already completed (e.g. `drop` acknowledged a write
    /// with nothing reaching the image); do not touch the image at all.
    DoneOk,
    /// Proceed, but the data to write/the buffer to serve has been
    /// replaced (tear, corrupt, stale, the data a reorder release
    /// carries).
    ProceedWith(Vec<u8>),
    /// Fail the request with this error, e.g. from `error` or `cut`.
    Fail(ErrorCode),
}

/// xorshift64*; deterministic, no host entropy -- the same shape
/// `drive_sounds.rs`'s `next_rng` uses for xorshift32, just widened so a
/// `probability`/`bits`/reorder-release draw has full `u64` range to work
/// with. Never seeded from wall-clock time: callers always derive the seed
/// from the policy's own `seed` and the target's `seq`, per this module's
/// doc comment.
fn next_rng(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn rng_for(seed: u64, seq: u64, draw: u64) -> u64 {
    let mut state = seed ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ draw;
    if state == 0 {
        state = 0xDEAD_BEEF_CAFE_F00D;
    }
    next_rng(&mut state)
}

impl FaultPolicySet {
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    pub fn set(
        &mut self,
        id: Option<String>,
        policy_json: &serde_json::Value,
        seed: u64,
    ) -> Result<String, String> {
        if self.policies.len() >= MAX_POLICIES_PER_TARGET
            && !id
                .as_ref()
                .is_some_and(|id| self.policies.iter().any(|p| &p.id == id))
        {
            return Err(format!(
                "limit: at most {MAX_POLICIES_PER_TARGET} policies per target"
            ));
        }
        let rules_json = policy_json
            .get("rules")
            .and_then(serde_json::Value::as_array)
            .ok_or("policy.rules: missing or not an array")?;
        if rules_json.len() > MAX_RULES_PER_POLICY {
            return Err(format!(
                "limit: at most {MAX_RULES_PER_POLICY} rules per policy"
            ));
        }
        let rules = rules_json
            .iter()
            .enumerate()
            .map(|(i, r)| Rule::from_json(r, i))
            .collect::<Result<Vec<_>, _>>()?;
        let on_clear = match policy_json
            .get("on_clear")
            .and_then(serde_json::Value::as_str)
        {
            Some("drop") => OnClear::Drop,
            Some("release") | None => OnClear::Release,
            Some(other) => return Err(format!("on_clear: unknown {other:?}")),
        };
        let seed = policy_json
            .get("seed")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(seed);
        let id = id.unwrap_or_else(|| format!("f{}", self.policies.len() + 1));
        let policy = Policy {
            id: id.clone(),
            seed,
            on_clear,
            rules,
        };
        if let Some(existing) = self.policies.iter_mut().find(|p| p.id == id) {
            *existing = policy;
        } else {
            self.policies.push(policy);
        }
        Ok(id)
    }

    /// Returns the ids actually cleared. `release` policies' held writes
    /// are returned so the caller can replay them onto the image in
    /// order (the proposal: "Held writes in a reorder window are
    /// released in order on clear unless the policy's `on_clear` says
    /// `drop`").
    pub fn clear(&mut self, id: Option<&str>) -> (Vec<String>, Vec<HeldWrite>) {
        let mut cleared = Vec::new();
        let mut released = Vec::new();
        self.policies.retain_mut(|p| {
            if id.is_some_and(|id| id != p.id) {
                return true;
            }
            cleared.push(p.id.clone());
            for rule in &mut p.rules {
                if matches!(p.on_clear, OnClear::Release) {
                    released.extend(rule.state.reorder_queue.drain(..));
                }
            }
            false
        });
        if cleared.is_empty() {
            return (cleared, released);
        }
        if self.policies.is_empty() {
            self.cut = false;
        }
        (cleared, released)
    }

    pub fn reset_stats(&mut self) {
        self.stats = Stats::default();
    }

    /// Finds the first matching, still-dormant-or-live rule across every
    /// installed policy, in installation order, matching the proposal's
    /// "the first matching rule whose probability test passes applies its
    /// action and stops evaluation unless it has `continue: true`" --
    /// across the whole policy set, not per-policy, since several
    /// policies may be active on one target at once.
    fn find_rule(&mut self, op: Op, lba: u64, count: u64, draw_base: u64) -> Vec<(usize, usize)> {
        let mut applied = Vec::new();
        let mut draw = draw_base;
        for (pi, policy) in self.policies.iter_mut().enumerate() {
            for (ri, rule) in policy.rules.iter_mut().enumerate() {
                if !rule.op.matches(op) {
                    continue;
                }
                if let Some(range) = rule.lba {
                    if op != Op::Any && !range.overlaps(lba, count) {
                        continue;
                    }
                }
                if let Some(times) = rule.times {
                    if rule.state.applied >= times {
                        continue;
                    }
                }
                // `self.stats.*` already counts *this* request (`bump_stats`
                // runs before `find_rule`), so "dormant until N have been
                // seen" is `n <= a.n`, not `n < a.n`: at `a.n == 2`, the
                // rule must stay dormant through the 2nd request (now
                // counted) and only go live on the 3rd.
                let dormant = rule.after.is_some_and(|a| {
                    let n = match a.kind {
                        AfterKind::Requests => self.stats.requests,
                        AfterKind::Reads => self.stats.reads,
                        AfterKind::Writes => self.stats.writes,
                        AfterKind::Flushes => self.stats.flushes,
                    };
                    n <= a.n
                });
                if dormant {
                    continue;
                }
                let passed = rule.probability >= 1.0 || {
                    let r = rng_for(policy.seed, self.seq, draw);
                    draw = draw.wrapping_add(1);
                    (r as f64 / u64::MAX as f64) < rule.probability
                };
                if !passed {
                    continue;
                }
                rule.state.applied += 1;
                rule.state.hits += 1;
                applied.push((pi, ri));
                if !rule.keep_going {
                    return applied;
                }
            }
        }
        applied
    }

    fn rule_mut(&mut self, loc: (usize, usize)) -> &mut Rule {
        &mut self.policies[loc.0].rules[loc.1]
    }

    fn policy_seed(&self, loc: (usize, usize)) -> u64 {
        self.policies[loc.0].seed
    }

    /// Called before any real I/O for every request kind, so `cut`
    /// (which can be matched by `op: any`) and stats bookkeeping are
    /// uniform across read/write/flush/trim.
    fn bump_stats(&mut self, op: Op, lba: u64, count: u64) {
        self.seq += 1;
        self.stats.requests += 1;
        match op {
            Op::Read => self.stats.reads += 1,
            Op::Write => self.stats.writes += 1,
            Op::Flush => self.stats.flushes += 1,
            Op::Trim => self.stats.trims += 1,
            Op::Any => {}
        }
        let _ = (lba, count);
    }

    fn apply_corrupt(&self, bits: u32, data: &mut [u8], seed: u64, seq: u64) {
        if data.is_empty() {
            return;
        }
        for b in 0..bits {
            let r = rng_for(seed, seq, 0x1000 + u64::from(b));
            let bit = (r as usize) % (data.len() * 8);
            data[bit / 8] ^= 1 << (bit % 8);
        }
    }

    /// Handles one write request: `lba`/`data` name the whole request
    /// (one or more consecutive sectors). Returns what the caller should
    /// do and, via `events`, one `(id, rule_index, action)` tuple per
    /// rule that actually fired (for the `disk.fault` event family and
    /// `disk.fault.list`'s hit counts -- `list`'s counts come from
    /// `Rule::state.hits` directly, `events` is for `events.subscribe`).
    #[allow(clippy::too_many_arguments)]
    pub fn on_write(&mut self, lba: u64, data: &[u8], sector_bytes: usize) -> Outcome {
        if self.cut {
            return Outcome::Fail(ErrorCode::NotReady);
        }
        let count = (data.len() / sector_bytes).max(1) as u64;
        self.bump_stats(Op::Write, lba, count);
        let seq = self.seq;
        let locs = self.find_rule(Op::Write, lba, count, seq.wrapping_mul(7));
        let mut out = data.to_vec();
        let mut outcome = Outcome::Proceed;
        for loc in locs {
            let seed = self.policy_seed(loc);
            let id = self.policies[loc.0].id.clone();
            let action = self.rule_mut(loc).action.clone();
            self.pending_events.push(FaultEvent {
                id,
                rule: loc.1,
                action: action.name().to_string(),
                op: "write".into(),
                lba,
                count,
                seq,
            });
            match action {
                Action::Drop => {
                    self.stats.dropped += 1;
                    outcome = Outcome::DoneOk;
                }
                Action::Tear { at, n } => {
                    self.stats.torn += 1;
                    let keep = match at {
                        TearAt::Sector => (n as usize * sector_bytes).min(out.len()),
                        TearAt::Bytes => (n as usize).min(out.len()),
                    };
                    out.truncate(keep);
                    outcome = if out.is_empty() {
                        Outcome::DoneOk
                    } else {
                        Outcome::ProceedWith(out.clone())
                    };
                }
                Action::Reorder { window } => {
                    self.stats.reordered += 1;
                    let rule = self.rule_mut(loc);
                    rule.state.reorder_queue.push_back(HeldWrite {
                        lba,
                        data: out.clone(),
                    });
                    if rule.state.reorder_queue.len() > window {
                        let evict =
                            (rng_for(seed, seq, 0x2000) as usize) % rule.state.reorder_queue.len();
                        rule.state.reorder_queue.remove(evict);
                    }
                    outcome = Outcome::DoneOk;
                }
                Action::Corrupt { bits } => {
                    self.stats.corrupted += 1;
                    self.apply_corrupt(bits, &mut out, seed, seq);
                    outcome = Outcome::ProceedWith(out.clone());
                }
                Action::Error { code } => {
                    self.stats.errored += 1;
                    return Outcome::Fail(code);
                }
                Action::Delay { ms } => {
                    self.stats.delayed_ms += ms;
                }
                Action::Cut => {
                    self.cut = true;
                    self.stats.cut = true;
                    for p in &mut self.policies {
                        for r in &mut p.rules {
                            r.state.reorder_queue.clear();
                        }
                    }
                    return Outcome::Fail(ErrorCode::NotReady);
                }
                Action::Ignore { .. } | Action::Stale { .. } => {}
            }
        }
        outcome
    }

    pub fn on_read(&mut self, lba: u64, data: &mut [u8], sector_bytes: usize) -> Outcome {
        if self.cut {
            return Outcome::Fail(ErrorCode::NotReady);
        }
        let count = (data.len() / sector_bytes).max(1) as u64;
        self.bump_stats(Op::Read, lba, count);
        let seq = self.seq;
        // Reads check every policy's reorder queue for this lba first
        // (a write cache would answer from itself), independent of
        // whether a `read` rule also matches -- the proposal: "Reads of
        // a held sector return the held data, as a write cache would."
        for policy in &self.policies {
            for rule in &policy.rules {
                if let Some(held) = rule.state.reorder_queue.iter().rev().find(|w| w.lba == lba) {
                    let n = held.data.len().min(data.len());
                    data[..n].copy_from_slice(&held.data[..n]);
                    return Outcome::ProceedWith(data.to_vec());
                }
            }
        }
        let locs = self.find_rule(Op::Read, lba, count, seq.wrapping_mul(7).wrapping_add(1));
        let mut out = data.to_vec();
        let mut outcome = Outcome::Proceed;
        for loc in locs {
            let seed = self.policy_seed(loc);
            let id = self.policies[loc.0].id.clone();
            let action = self.rule_mut(loc).action.clone();
            self.pending_events.push(FaultEvent {
                id,
                rule: loc.1,
                action: action.name().to_string(),
                op: "read".into(),
                lba,
                count,
                seq,
            });
            match action {
                Action::Corrupt { bits } => {
                    self.stats.corrupted += 1;
                    self.apply_corrupt(bits, &mut out, seed, seq);
                    outcome = Outcome::ProceedWith(out.clone());
                }
                Action::Error { code } => {
                    self.stats.errored += 1;
                    return Outcome::Fail(code);
                }
                Action::Delay { ms } => {
                    self.stats.delayed_ms += ms;
                }
                Action::Cut => {
                    self.cut = true;
                    self.stats.cut = true;
                    return Outcome::Fail(ErrorCode::NotReady);
                }
                _ => {}
            }
        }
        outcome
    }

    /// `stale`'s own trigger is "after a sector is written" rather than a
    /// direct read-time rule match, so [`Self::on_write`] calls this once
    /// the real write has landed (the caller's responsibility -- see
    /// `HardDriveImage::write_sector`) to arm the pending-stale record,
    /// and [`Self::on_read`] would need to consult it; wiring the actual
    /// read-time substitution is left to a follow-up once a real `stale`
    /// test exists to verify the arm/consume sequencing against, per this
    /// phase's scope (flagged in the implementation report rather than
    /// guessed at silently).
    pub fn arm_stale(&mut self, _lba: u64, _pre_write: &[u8], _count: u64) {}

    pub fn on_flush(&mut self) -> Outcome {
        if self.cut {
            return Outcome::Fail(ErrorCode::NotReady);
        }
        self.bump_stats(Op::Flush, 0, 0);
        let seq = self.seq;
        let locs = self.find_rule(Op::Flush, 0, 0, seq.wrapping_mul(7).wrapping_add(2));
        let mut outcome = Outcome::Proceed;
        for loc in locs {
            let id = self.policies[loc.0].id.clone();
            let action = self.rule_mut(loc).action.clone();
            self.pending_events.push(FaultEvent {
                id,
                rule: loc.1,
                action: action.name().to_string(),
                op: "flush".into(),
                lba: 0,
                count: 0,
                seq,
            });
            match action {
                Action::Ignore { then } => {
                    self.stats.flush_ignored += 1;
                    if matches!(then, IgnoreThen::Drop) {
                        for p in &mut self.policies {
                            for r in &mut p.rules {
                                r.state.reorder_queue.clear();
                            }
                        }
                    }
                    outcome = Outcome::DoneOk;
                }
                Action::Error { code } => {
                    self.stats.errored += 1;
                    return Outcome::Fail(code);
                }
                Action::Delay { ms } => {
                    self.stats.delayed_ms += ms;
                }
                Action::Cut => {
                    self.cut = true;
                    self.stats.cut = true;
                    return Outcome::Fail(ErrorCode::NotReady);
                }
                _ => {}
            }
        }
        if matches!(outcome, Outcome::Proceed) {
            // A flush with no `ignore` rule releases every policy's held
            // queue, in a seeded random order (not FIFO) -- the proposal's
            // "A flush releases the whole queue in a seeded random order".
            // The caller (`HardDriveImage::flush`) drains
            // `take_release_on_flush` after this returns `Proceed` and
            // replays each held write for real.
        }
        outcome
    }

    /// Drains every policy's reorder queue in a seeded random order, for
    /// `HardDriveImage::flush` to replay onto the image once
    /// [`Self::on_flush`] has returned `Proceed` (not `ignore`d).
    pub fn take_release_on_flush(&mut self) -> Vec<HeldWrite> {
        let mut out = Vec::new();
        let seq = self.seq;
        for policy in &mut self.policies {
            for rule in &mut policy.rules {
                let mut items: Vec<_> = rule.state.reorder_queue.drain(..).collect();
                let mut i = items.len();
                while i > 1 {
                    i -= 1;
                    let j = (rng_for(policy.seed, seq, 0x3000 + i as u64) as usize) % (i + 1);
                    items.swap(i, j);
                }
                out.extend(items);
            }
        }
        out
    }
}

/// A `disk.fault.*` `target` parameter, resolved by
/// `Bus::with_fault_policy_mut` (every kind) and `Bus::
/// with_hard_drive_image_mut` (`copperhf`/`pcmcia`/`ide`/`scsi`, which
/// also need real I/O capability for `disk.fault.clear`'s held-write
/// replay, not just the policy set). `Floppy` additionally needs
/// `FloppyController::drive_has_sector_model` checked at `disk.fault.set`
/// time: a `Tracks`-backed (extended ADF/raw MFM) image has no sector
/// grid to fault, so installing a policy on one would silently never
/// fire rather than erroring -- see `FloppyImage::has_sector_model`'s doc
/// comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiskFaultTarget {
    Copperhf { unit: usize },
    Pcmcia,
    Ide { unit: usize },
    Scsi { unit: usize },
    Floppy { drive: usize },
    Cd,
}

impl DiskFaultTarget {
    pub fn from_json(v: &serde_json::Value) -> Result<DiskFaultTarget, String> {
        let kind = v
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or("target.kind: missing")?;
        let unit = || {
            v.get("unit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize
        };
        Ok(match kind {
            "copperhf" => DiskFaultTarget::Copperhf { unit: unit() },
            "pcmcia" => DiskFaultTarget::Pcmcia,
            "ide" => DiskFaultTarget::Ide { unit: unit() },
            "scsi" => DiskFaultTarget::Scsi { unit: unit() },
            "floppy" => DiskFaultTarget::Floppy {
                drive: v
                    .get("drive")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize,
            },
            "cd" => DiskFaultTarget::Cd,
            other => return Err(format!("target.kind: unknown {other:?}")),
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            DiskFaultTarget::Copperhf { unit } => {
                serde_json::json!({"kind": "copperhf", "unit": unit})
            }
            DiskFaultTarget::Pcmcia => serde_json::json!({"kind": "pcmcia"}),
            DiskFaultTarget::Ide { unit } => serde_json::json!({"kind": "ide", "unit": unit}),
            DiskFaultTarget::Scsi { unit } => serde_json::json!({"kind": "scsi", "unit": unit}),
            DiskFaultTarget::Floppy { drive } => {
                serde_json::json!({"kind": "floppy", "drive": drive})
            }
            DiskFaultTarget::Cd => serde_json::json!({"kind": "cd"}),
        }
    }
}

/// One `disk.fault` event (`events.subscribe`'s `disk.fault` family).
#[derive(Clone, Debug)]
pub struct FaultEvent {
    pub id: String,
    pub rule: usize,
    pub action: String,
    pub op: String,
    pub lba: u64,
    pub count: u64,
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(rules: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"rules": rules})
    }

    #[test]
    fn drop_acknowledges_without_touching_the_image() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([{"op": "write", "action": "drop", "probability": 1.0}])),
            1,
        )
        .unwrap();
        let outcome = set.on_write(0, &[0u8; 512], 512);
        assert!(matches!(outcome, Outcome::DoneOk));
        assert_eq!(set.stats.dropped, 1);
        assert_eq!(set.pending_events.len(), 1);
    }

    #[test]
    fn tear_truncates_to_n_sectors() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([{"op": "write", "action": "tear", "at": "sector", "n": 1}])),
            1,
        )
        .unwrap();
        let data = vec![0xAAu8; 1024];
        let outcome = set.on_write(0, &data, 512);
        match outcome {
            Outcome::ProceedWith(out) => assert_eq!(out.len(), 512),
            other => panic!(
                "expected ProceedWith, got a different outcome: {}",
                matches!(other, Outcome::Proceed)
            ),
        }
        assert_eq!(set.stats.torn, 1);
    }

    #[test]
    fn tear_at_n_zero_is_equivalent_to_drop() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([{"op": "write", "action": "tear", "at": "sector", "n": 0}])),
            1,
        )
        .unwrap();
        let outcome = set.on_write(0, &[0xAAu8; 512], 512);
        assert!(matches!(outcome, Outcome::DoneOk));
    }

    #[test]
    fn times_retires_a_rule_after_its_budget() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(
                serde_json::json!([{"op": "read", "action": "error", "code": "media", "times": 2}]),
            ),
            1,
        )
        .unwrap();
        assert!(matches!(
            set.on_read(0, &mut [0u8; 512], 512),
            Outcome::Fail(ErrorCode::Media)
        ));
        assert!(matches!(
            set.on_read(0, &mut [0u8; 512], 512),
            Outcome::Fail(ErrorCode::Media)
        ));
        assert!(matches!(
            set.on_read(0, &mut [0u8; 512], 512),
            Outcome::Proceed
        ));
    }

    #[test]
    fn after_gate_stays_dormant_until_the_request_count_is_reached() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "drop", "after": {"writes": 2}}
            ])),
            1,
        )
        .unwrap();
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512),
            Outcome::Proceed
        ));
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512),
            Outcome::Proceed
        ));
        assert!(matches!(set.on_write(0, &[0u8; 512], 512), Outcome::DoneOk));
    }

    #[test]
    fn cut_fails_this_and_every_later_request_until_cleared() {
        let mut set = FaultPolicySet::default();
        set.set(None, &policy(serde_json::json!([{"action": "cut"}])), 1)
            .unwrap();
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512),
            Outcome::Fail(ErrorCode::NotReady)
        ));
        assert!(matches!(
            set.on_read(0, &mut [0u8; 512], 512),
            Outcome::Fail(ErrorCode::NotReady)
        ));
        set.clear(None);
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512),
            Outcome::Proceed
        ));
    }

    #[test]
    fn same_seed_and_requests_fault_identically_twice() {
        let run = || {
            let mut set = FaultPolicySet::default();
            set.set(
                None,
                &policy(serde_json::json!([
                    {"op": "write", "action": "drop", "probability": 0.5}
                ])),
                42,
            )
            .unwrap();
            (0..50)
                .map(|i| matches!(set.on_write(i, &[0u8; 512], 512), Outcome::DoneOk))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn reorder_holds_writes_and_read_sees_the_held_data() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([{"op": "write", "action": "reorder", "window": 4}])),
            1,
        )
        .unwrap();
        let new_data = vec![0x42u8; 512];
        assert!(matches!(set.on_write(5, &new_data, 512), Outcome::DoneOk));
        assert_eq!(set.stats.reordered, 1);
        let mut buf = vec![0u8; 512];
        let outcome = set.on_read(5, &mut buf, 512);
        assert!(matches!(outcome, Outcome::ProceedWith(_)));
        assert_eq!(buf, new_data);
    }

    #[test]
    fn flush_release_drains_the_reorder_queue() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([{"op": "write", "action": "reorder", "window": 4}])),
            1,
        )
        .unwrap();
        set.on_write(1, &[1u8; 512], 512);
        set.on_write(2, &[2u8; 512], 512);
        assert!(matches!(set.on_flush(), Outcome::Proceed));
        let released = set.take_release_on_flush();
        assert_eq!(released.len(), 2);
    }

    #[test]
    fn limits_are_enforced() {
        let mut set = FaultPolicySet::default();
        for i in 0..MAX_POLICIES_PER_TARGET {
            set.set(Some(format!("p{i}")), &policy(serde_json::json!([])), 1)
                .unwrap();
        }
        assert!(set
            .set(
                Some("one-too-many".into()),
                &policy(serde_json::json!([])),
                1
            )
            .is_err());
        let mut too_many_rules = Vec::new();
        for _ in 0..=MAX_RULES_PER_POLICY {
            too_many_rules.push(serde_json::json!({"action": "drop"}));
        }
        let mut fresh = FaultPolicySet::default();
        assert!(fresh
            .set(None, &policy(serde_json::Value::Array(too_many_rules)), 1)
            .is_err());
    }
}
