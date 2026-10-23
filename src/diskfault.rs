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
/// A `corrupt` rule's `bits`, bounded well past any real sector's bit
/// count -- see the `"corrupt"` arm of `Rule::from_json`'s doc comment.
pub const MAX_CORRUPT_BITS: u64 = 65536;
/// `pending_events` is bounded to this many, oldest dropped first -- no
/// `events.subscribe` wiring drains it yet (see `FaultPolicySet::
/// pending_events`'s own doc comment), so without a cap a long soak run
/// with a frequently-matching rule would grow it without bound.
pub const MAX_PENDING_EVENTS: usize = 256;

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

/// Carries an `ErrorCode` as the payload of the error a fault `error`/
/// `cut` action produces (`HardDriveImage`/`CdImage`'s `fault_io_error`),
/// so a controller that wants the exact code -- not just "this request
/// failed" -- can recover it with [`fault_error_code`]/
/// [`fault_error_code_anyhow`] instead of guessing from error text or
/// collapsing every code to one generic failure.
#[derive(Debug, Clone, Copy)]
pub struct FaultIoError(pub ErrorCode);

impl std::fmt::Display for FaultIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "disk fault: {}", self.0.as_str())
    }
}

impl std::error::Error for FaultIoError {}

/// Recovers the exact `ErrorCode` from an `io::Error` a fault action
/// produced, if `err` actually came from one -- `None` for an ordinary
/// I/O failure (a real disk error, a session being read-only, ...),
/// which a caller should keep treating as a generic failure.
pub fn fault_error_code(err: &std::io::Error) -> Option<ErrorCode> {
    err.get_ref()?.downcast_ref::<FaultIoError>().map(|e| e.0)
}

/// [`fault_error_code`] for the `anyhow::Result` callers on the `cd`
/// target's read path use (`CdImage::read_data_sector` and friends)
/// instead of `std::io::Result`.
pub fn fault_error_code_anyhow(err: &anyhow::Error) -> Option<ErrorCode> {
    err.downcast_ref::<FaultIoError>().map(|e| e.0)
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
    /// bytes to keep serving). `BTreeMap`, not a hash map: this
    /// round-trips through a save state, and `HashMap`'s per-process
    /// `RandomState` would make two saves of the same machine serialize
    /// differently once two or more sectors are armed -- the same defect
    /// the blitter's `d_overlay` had, fixed the same way.
    pub stale_pending: std::collections::BTreeMap<u64, (u64, Vec<u8>)>,
    /// `after`'s gate: the target-wide stat (in `after.kind`'s units) at
    /// the moment this rule was installed, so "after N requests" counts
    /// from installation, not from whatever the shared counter already
    /// read (a policy installed after 1000 prior requests must still
    /// wait its own N, not go live immediately) or from the last
    /// `disk.fault.reset_stats` (which must not affect installed
    /// policies). `0` for a rule with no `after` or loaded from a state
    /// saved before this field existed -- the latter only ever makes an
    /// already-satisfied gate look satisfied a little earlier, never the
    /// reverse.
    #[serde(default)]
    pub after_baseline: u64,
    /// Latches once `after`'s gate is satisfied, so a later
    /// `disk.fault.reset_stats` zeroing the shared counters can never
    /// make an already-live rule look dormant again.
    #[serde(default)]
    pub after_satisfied: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HeldWrite {
    pub lba: u64,
    pub data: Vec<u8>,
    /// The target's request-sequence number at the moment this write was
    /// held, so a release that flattens several rules' queues together
    /// can tell which of two holds to the same `lba` is newer -- see
    /// `take_release_on_flush`'s doc comment. `0` for a state saved
    /// before this field existed (the oldest possible value, so an old
    /// held write never wins a dedup against a freshly-held one after a
    /// state.load).
    #[serde(default)]
    pub seq: u64,
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
                    .unwrap_or(1);
                // `apply_corrupt` draws and flips one bit per iteration --
                // a value anywhere near `u32::MAX` loops billions of times
                // per hit and hangs the emulator, so this is bounded well
                // past any real sector's bit count (the largest sector
                // this engine faults is a few KiB) rather than accepted
                // as schema-valid and left to the caller to discover the
                // hang.
                if bits > MAX_CORRUPT_BITS {
                    return Err(err("bits", &format!("must be at most {MAX_CORRUPT_BITS}")));
                }
                Action::Corrupt { bits: bits as u32 }
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
    /// Meant for the control layer to drain for `events.subscribe`'s
    /// `disk.fault` family (not yet wired up) -- not state (never saved/
    /// loaded), so it is skipped rather than round-tripped through a save
    /// state the way `policies`/`seq` are. Bounded to
    /// [`MAX_PENDING_EVENTS`], oldest dropped first, since nothing drains
    /// it today.
    #[serde(skip)]
    pub pending_events: VecDeque<FaultEvent>,
    /// Set by `cut`: every request fails `not_ready` until an explicit
    /// `disk.fault.clear`, even across a `machine.reset` (the proposal's
    /// "power-cut recovery test" -- a reset must not quietly un-cut the
    /// drive). The single source of truth for cut state -- `disk.fault.
    /// stats`'s `cut` field reports this directly rather than keeping a
    /// second copy in `Stats`, so `reset_stats` can never desync one from
    /// the other.
    pub cut: bool,
    /// The id of the policy whose rule fired `cut`, so `clear` can tell
    /// whether the policy being removed is the one actually holding the
    /// latch (as opposed to an unrelated policy that merely happens to
    /// share the target). `None` for a state saved before this field
    /// existed, or if cut is false.
    #[serde(default)]
    pub cut_by: Option<String>,
    /// A `reorder` window overflow's evicted write, queued here for the
    /// caller to commit for real (see `take_evicted`'s doc comment) --
    /// not state (never saved/loaded): the caller drains this in the
    /// same call that produced it, before any save state could land in
    /// between.
    #[serde(skip)]
    pending_evicted: VecDeque<HeldWrite>,
    /// A `delay` fired during the most recent `on_write`/`on_read`/
    /// `on_flush` call, in milliseconds -- not state (never saved/
    /// loaded): a controller able to add real completion latency (not
    /// every one is; see [`Self::take_delay_ms`]'s doc comment) drains
    /// this right after the call that produced it, same shape as
    /// `take_evicted`.
    #[serde(skip)]
    pending_delay_ms: u64,
}

/// What a caller (`HardDriveImage::read_sector` and friends) must do with
/// one request after the policy set has had a chance to act on it.
#[derive(Debug)]
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

/// A free function, not a method: touches no `FaultPolicySet` field, and
/// a `deferred_at` continuation needs to corrupt a buffer it already
/// holds a mutable borrow into (via `deferred_write_mut`) alongside a
/// `&self` call, which an `&self` method here would conflict with.
fn apply_corrupt(bits: u32, data: &mut [u8], seed: u64, seq: u64) {
    if data.is_empty() {
        return;
    }
    for b in 0..bits {
        let r = rng_for(seed, seq, 0x1000 + u64::from(b));
        let bit = (r as usize) % (data.len() * 8);
        data[bit / 8] ^= 1 << (bit % 8);
    }
}

impl FaultPolicySet {
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Returns the policy's id and, when `set` *replaces* an existing
    /// policy at that id, the old policy's held reorder-queue writes the
    /// caller must replay onto the image (same `on_clear` semantics as
    /// [`Self::clear`] -- `release` returns them, `drop` discards them),
    /// so replacing a policy's rules mid-session can no longer silently
    /// lose writes the guest already saw succeed and read back.
    pub fn set(
        &mut self,
        id: Option<String>,
        policy_json: &serde_json::Value,
        seed: u64,
    ) -> Result<(String, Vec<HeldWrite>), String> {
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
        let mut rules = rules_json
            .iter()
            .enumerate()
            .map(|(i, r)| Rule::from_json(r, i))
            .collect::<Result<Vec<_>, _>>()?;
        // `after`'s gate counts from installation: snapshot the relevant
        // shared counter now so `find_rule`'s dormancy check can measure
        // elapsed requests from here, not from zero or from whatever the
        // counter already read.
        for rule in &mut rules {
            if let Some(after) = rule.after {
                rule.state.after_baseline = match after.kind {
                    AfterKind::Requests => self.stats.requests,
                    AfterKind::Reads => self.stats.reads,
                    AfterKind::Writes => self.stats.writes,
                    AfterKind::Flushes => self.stats.flushes,
                };
            }
        }
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
        // Not just `f{len + 1}`: once policies have been added and removed
        // out of order that is no longer collision-free (e.g. [f1, f3]
        // after clearing f2 has len 2, so the next auto id would recompute
        // f3 and silently overwrite the existing one below). Scan forward
        // from there instead, skipping any id -- auto- or explicitly-
        // assigned -- already in use.
        let id = id.unwrap_or_else(|| {
            let mut n = self.policies.len() + 1;
            loop {
                let candidate = format!("f{n}");
                if !self.policies.iter().any(|p| p.id == candidate) {
                    break candidate;
                }
                n += 1;
            }
        });
        let policy = Policy {
            id: id.clone(),
            seed,
            on_clear,
            rules,
        };
        let mut released = Vec::new();
        if let Some(existing) = self.policies.iter_mut().find(|p| p.id == id) {
            if matches!(existing.on_clear, OnClear::Release) {
                for rule in &mut existing.rules {
                    released.extend(rule.state.reorder_queue.drain(..));
                }
            }
            *existing = policy;
        } else {
            self.policies.push(policy);
        }
        Ok((id, released))
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
        // Un-cut when either the policy that actually caused it is among
        // those just cleared, or (for a cut latched before `cut_by`
        // existed, or by some policy no longer present) the whole set is
        // now empty -- never on an unrelated policy's clear while the
        // cutting one is still installed.
        if self.policies.is_empty()
            || self
                .cut_by
                .as_deref()
                .is_some_and(|by| cleared.iter().any(|id| id == by))
        {
            self.cut = false;
            self.cut_by = None;
        }
        (cleared, released)
    }

    pub fn reset_stats(&mut self) {
        self.stats = Stats::default();
    }

    /// `cut` models the volatile write cache being lost in a power cut,
    /// whichever op's rule actually fired it -- a `write` rule's `Cut`
    /// arm is not the only path that can latch `cut`, so every held
    /// reorder write and armed `stale` entry is discarded here, shared by
    /// all three `on_write`/`on_read`/`on_flush` `Cut` arms. Without this,
    /// a cut latched by a `read` or `flush` rule would leave the queues
    /// full, and a later `disk.fault.clear` with `on_clear: release`
    /// would replay data that should have been lost in the cut onto the
    /// disk -- exactly what a power-loss recovery test is meant to catch.
    fn discard_held_state_on_cut(&mut self) {
        for p in &mut self.policies {
            for r in &mut p.rules {
                r.state.reorder_queue.clear();
                r.state.stale_pending.clear();
            }
        }
    }

    /// Records a fired-rule event, dropping the oldest once
    /// [`MAX_PENDING_EVENTS`] are queued -- see `pending_events`'s own doc
    /// comment for why this needs a cap at all.
    fn push_event(&mut self, event: FaultEvent) {
        if self.pending_events.len() >= MAX_PENDING_EVENTS {
            self.pending_events.pop_front();
        }
        self.pending_events.push_back(event);
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
                // `op` here is always the caller's fixed request kind
                // (`on_write`/`on_read`/`on_flush` each hardcode their own
                // `Op::Write`/`Read`/`Flush`), never `Op::Any` -- a `lba`
                // range always applies.
                if let Some(range) = rule.lba {
                    if !range.overlaps(lba, count) {
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
                // seen" is `elapsed <= a.n`, not `elapsed < a.n`: at
                // `a.n == 2`, the rule must stay dormant through the 2nd
                // request since installation (now counted) and only go
                // live on the 3rd. `elapsed` counts from `after_baseline`
                // (this rule's installation), not from zero -- and once
                // satisfied, `after_satisfied` latches permanently so a
                // later `disk.fault.reset_stats` can't undo it (see
                // `RuleState::after_satisfied`'s doc comment).
                let dormant = !rule.state.after_satisfied
                    && rule.after.is_some_and(|a| {
                        let n = match a.kind {
                            AfterKind::Requests => self.stats.requests,
                            AfterKind::Reads => self.stats.reads,
                            AfterKind::Writes => self.stats.writes,
                            AfterKind::Flushes => self.stats.flushes,
                        };
                        let elapsed = n.saturating_sub(rule.state.after_baseline);
                        if elapsed > a.n {
                            rule.state.after_satisfied = true;
                            false
                        } else {
                            true
                        }
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
    fn bump_stats(&mut self, op: Op) {
        self.seq += 1;
        self.stats.requests += 1;
        match op {
            Op::Read => self.stats.reads += 1,
            Op::Write => self.stats.writes += 1,
            Op::Flush => self.stats.flushes += 1,
            Op::Trim => self.stats.trims += 1,
            Op::Any => {}
        }
    }

    /// Handles one write request: `lba`/`data` name the whole request
    /// (one or more consecutive sectors). Returns what the caller should
    /// do and, via `events`, one `(id, rule_index, action)` tuple per
    /// rule that actually fired (for the `disk.fault` event family and
    /// `disk.fault.list`'s hit counts -- `list`'s counts come from
    /// `Rule::state.hits` directly, `events` is for `events.subscribe`).
    /// `pre_write` is the sector's content immediately before this write
    /// commits -- the caller's responsibility to read, since only it
    /// knows how (and whether it already has to, e.g. `tear`'s `at:
    /// bytes` splice reads it too). Only consulted for a matched `Stale`
    /// rule, to arm what `on_read` serves back; every other action
    /// ignores it. `data`/`sector_bytes` describe exactly one sector per
    /// call in every current caller (`HardDriveImage::write_sector`,
    /// the floppy write hook's per-sector loop), so `stale_pending` is
    /// keyed on this call's single `lba` -- a future multi-sector caller
    /// would need to call this once per sector, not once per request.
    pub fn on_write(
        &mut self,
        lba: u64,
        data: &[u8],
        sector_bytes: usize,
        pre_write: &[u8],
    ) -> Outcome {
        if self.cut {
            return Outcome::Fail(ErrorCode::NotReady);
        }
        let count = (data.len() / sector_bytes).max(1) as u64;
        self.bump_stats(Op::Write);
        let seq = self.seq;
        let locs = self.find_rule(Op::Write, lba, count, seq.wrapping_mul(7));
        let mut out = data.to_vec();
        let mut outcome = Outcome::Proceed;
        // Two of `continue: true`'s rules fix what a chain of actions
        // means when they disagree about whether this write ever really
        // reaches the image:
        //
        // `dropped_for_good`: once `drop` fires, the write is gone for
        // good -- a later action that would otherwise resurrect it
        // (`reorder` queuing a copy for a future flush, `corrupt`/`tear`
        // producing a `ProceedWith`) is a no-op instead, and `drop` at
        // any later point in the chain also discards whatever an
        // earlier `reorder` already queued.
        //
        // `deferred_at`: once `reorder` fires, the write is already
        // queued (not an independent real commit) -- a later `corrupt`
        // mutates *that queued copy* in place instead of also producing
        // its own `ProceedWith` (which was landing a second, overwriting
        // copy directly while the queue still held a stale one), and a
        // later `error` removes this call's own queued copy before
        // failing (a command the guest sees fail never left anything in
        // a real drive's write cache for a later flush to resurrect).
        let mut dropped_for_good = false;
        let mut deferred_at: Option<(usize, usize)> = None;
        for loc in locs {
            let seed = self.policy_seed(loc);
            let id = self.policies[loc.0].id.clone();
            let action = self.rule_mut(loc).action.clone();
            self.push_event(FaultEvent {
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
                    if let Some(at) = deferred_at.take() {
                        self.remove_deferred_write(at, lba, seq);
                    }
                    dropped_for_good = true;
                    outcome = Outcome::DoneOk;
                }
                Action::Tear { at, n } => {
                    self.stats.torn += 1;
                    // `dropped_for_good`: already gone, nothing left to
                    // tear. `deferred_at`: an earlier `reorder` in this
                    // chain already queued the write and owns its fate
                    // from here -- a safe no-op rather than also
                    // reaching into the queued copy, which `reorder`
                    // itself already handles correctly in the opposite
                    // order (`tear` then `reorder` truncates `out` first,
                    // and `reorder` queues whatever `out` holds by the
                    // time it runs).
                    if dropped_for_good || deferred_at.is_some() {
                        continue;
                    }
                    let keep = match at {
                        // `saturating_mul`, not `*`: an `n` anywhere near
                        // `u64::MAX` would otherwise overflow `usize`
                        // (panicking in a debug build) before the `.min`
                        // below ever gets a chance to clamp it down to
                        // something the buffer could actually hold.
                        TearAt::Sector => (n as usize).saturating_mul(sector_bytes).min(out.len()),
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
                    if dropped_for_good || deferred_at.is_some() {
                        continue;
                    }
                    // Always a whole number of sectors, even when an
                    // earlier `continue: true` action (`tear`) already
                    // truncated `out`: every release path
                    // (`take_release_on_flush`/`take_evicted`/
                    // `disk.fault.clear`) writes a held write's `data`
                    // sector-by-sector, and a buffer that doesn't end on
                    // a sector boundary panics there instead of
                    // torn-and-held. The untouched tail is spliced from
                    // `pre_write`, same reasoning as the top-level
                    // tear-tail splice in `HardDriveImage::write_sector`
                    // -- for a multi-sector request (`write_sectors`),
                    // `pre_write` spans the same range as `out`, so the
                    // splice lands in the right sector even when the cut
                    // falls past the first one.
                    let rounded = out.len().max(1).div_ceil(sector_bytes) * sector_bytes;
                    let held_data = if out.len() == rounded && !out.is_empty() {
                        out.clone()
                    } else {
                        let mut full = pre_write.to_vec();
                        full.resize(rounded, 0);
                        full[..out.len()].copy_from_slice(&out);
                        full
                    };
                    let rule = self.rule_mut(loc);
                    rule.state.reorder_queue.push_back(HeldWrite {
                        lba,
                        data: held_data,
                        seq,
                    });
                    deferred_at = Some(loc);
                    if rule.state.reorder_queue.len() > window {
                        let evict =
                            (rng_for(seed, seq, 0x2000) as usize) % rule.state.reorder_queue.len();
                        // Committed, not discarded: a real write cache
                        // full enough to evict still has to put the
                        // evicted dirty data somewhere, it does not throw
                        // it away. `on_write` has no I/O of its own
                        // (diskfault.rs touches no image), so the evicted
                        // write is queued here for the caller to commit
                        // for real -- see `take_evicted`'s doc comment.
                        if let Some(evicted) = rule.state.reorder_queue.remove(evict) {
                            // If the entry this call just pushed is the
                            // one evicted, there is no longer a queued
                            // copy for a later action in this chain to
                            // adjust.
                            if evicted.lba == lba && evicted.seq == seq {
                                deferred_at = None;
                            }
                            self.pending_evicted.push_back(evicted);
                        }
                    }
                    outcome = Outcome::DoneOk;
                }
                Action::Corrupt { bits } => {
                    self.stats.corrupted += 1;
                    if dropped_for_good {
                        continue;
                    }
                    if let Some(at) = deferred_at {
                        if let Some(held) = self.deferred_write_mut(at, lba, seq) {
                            apply_corrupt(bits, held, seed, seq);
                        }
                    } else {
                        apply_corrupt(bits, &mut out, seed, seq);
                        outcome = Outcome::ProceedWith(out.clone());
                    }
                }
                Action::Error { code } => {
                    self.stats.errored += 1;
                    if let Some(at) = deferred_at {
                        // The guest sees this write fail -- nothing a
                        // real drive's write cache held for it survives
                        // that, so this call's own queued copy (not any
                        // other write already legitimately held before
                        // it) must go too.
                        self.remove_deferred_write(at, lba, seq);
                    }
                    return Outcome::Fail(code);
                }
                Action::Delay { ms } => {
                    self.stats.delayed_ms += ms;
                    self.pending_delay_ms += ms;
                }
                Action::Cut => {
                    self.cut = true;
                    self.cut_by = Some(self.policies[loc.0].id.clone());
                    self.discard_held_state_on_cut();
                    return Outcome::Fail(ErrorCode::NotReady);
                }
                Action::Stale { count } => {
                    if count > 0 && !dropped_for_good {
                        self.rule_mut(loc)
                            .state
                            .stale_pending
                            .insert(lba, (count, pre_write.to_vec()));
                    }
                }
                Action::Ignore { .. } => {}
            }
        }
        outcome
    }

    /// The data of the held write this call's own `reorder` queued at
    /// `at` (a `(policy, rule)` index pair), if it is still there --
    /// `None` if a window eviction already released it before a later
    /// `continue: true` action in the same call got a chance to touch
    /// it. Identified by `(lba, seq)`, not just "whatever is at the back
    /// of the queue": an eviction can reorder what is actually last.
    fn deferred_write_mut(
        &mut self,
        at: (usize, usize),
        lba: u64,
        seq: u64,
    ) -> Option<&mut Vec<u8>> {
        self.policies
            .get_mut(at.0)?
            .rules
            .get_mut(at.1)?
            .state
            .reorder_queue
            .iter_mut()
            .rev()
            .find(|w| w.lba == lba && w.seq == seq)
            .map(|w| &mut w.data)
    }

    /// Removes this call's own held write from `at`, if still queued --
    /// see [`Self::deferred_write_mut`]'s doc comment on why eviction can
    /// make that conditional.
    fn remove_deferred_write(&mut self, at: (usize, usize), lba: u64, seq: u64) {
        let Some(policy) = self.policies.get_mut(at.0) else {
            return;
        };
        let Some(rule) = policy.rules.get_mut(at.1) else {
            return;
        };
        if let Some(pos) = rule
            .state
            .reorder_queue
            .iter()
            .rposition(|w| w.lba == lba && w.seq == seq)
        {
            rule.state.reorder_queue.remove(pos);
        }
    }

    pub fn on_read(&mut self, lba: u64, data: &mut [u8], sector_bytes: usize) -> Outcome {
        if self.cut {
            return Outcome::Fail(ErrorCode::NotReady);
        }
        let count = (data.len() / sector_bytes).max(1) as u64;
        self.bump_stats(Op::Read);
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
        // `stale` (armed by a prior matching write, `on_write`'s `Action::
        // Stale` arm): serves the pre-write bytes for `count` more reads,
        // then reverts to the committed data -- found first via an
        // immutable scan so the actual decrement/removal below doesn't
        // need a second mutable borrow of `self.policies` alongside it.
        let mut stale_hit = None;
        'find_stale: for (pi, policy) in self.policies.iter().enumerate() {
            for (ri, rule) in policy.rules.iter().enumerate() {
                if rule.state.stale_pending.contains_key(&lba) {
                    stale_hit = Some((pi, ri));
                    break 'find_stale;
                }
            }
        }
        if let Some((pi, ri)) = stale_hit {
            let rule = &mut self.policies[pi].rules[ri];
            let (remaining, bytes) = rule
                .state
                .stale_pending
                .get_mut(&lba)
                .expect("just found by the scan above");
            let n = bytes.len().min(data.len());
            let served = bytes[..n].to_vec();
            *remaining -= 1;
            if *remaining == 0 {
                rule.state.stale_pending.remove(&lba);
            }
            self.stats.stale_served += 1;
            return Outcome::ProceedWith(served);
        }
        let locs = self.find_rule(Op::Read, lba, count, seq.wrapping_mul(7).wrapping_add(1));
        let mut out = data.to_vec();
        let mut outcome = Outcome::Proceed;
        for loc in locs {
            let seed = self.policy_seed(loc);
            let id = self.policies[loc.0].id.clone();
            let action = self.rule_mut(loc).action.clone();
            self.push_event(FaultEvent {
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
                    apply_corrupt(bits, &mut out, seed, seq);
                    outcome = Outcome::ProceedWith(out.clone());
                }
                Action::Error { code } => {
                    self.stats.errored += 1;
                    return Outcome::Fail(code);
                }
                Action::Delay { ms } => {
                    self.stats.delayed_ms += ms;
                    self.pending_delay_ms += ms;
                }
                Action::Cut => {
                    self.cut = true;
                    self.cut_by = Some(self.policies[loc.0].id.clone());
                    self.discard_held_state_on_cut();
                    return Outcome::Fail(ErrorCode::NotReady);
                }
                _ => {}
            }
        }
        outcome
    }

    pub fn on_flush(&mut self) -> Outcome {
        if self.cut {
            return Outcome::Fail(ErrorCode::NotReady);
        }
        self.bump_stats(Op::Flush);
        let seq = self.seq;
        let locs = self.find_rule(Op::Flush, 0, 0, seq.wrapping_mul(7).wrapping_add(2));
        let mut outcome = Outcome::Proceed;
        for loc in locs {
            let id = self.policies[loc.0].id.clone();
            let action = self.rule_mut(loc).action.clone();
            self.push_event(FaultEvent {
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
                    self.pending_delay_ms += ms;
                }
                Action::Cut => {
                    self.cut = true;
                    self.cut_by = Some(self.policies[loc.0].id.clone());
                    self.discard_held_state_on_cut();
                    return Outcome::Fail(ErrorCode::NotReady);
                }
                _ => {}
            }
        }
        // A flush with no `ignore` rule releases every policy's held
        // queue, in a seeded random order (not FIFO) -- the proposal's "A
        // flush releases the whole queue in a seeded random order". The
        // caller (`HardDriveImage::flush`) drains `take_release_on_flush`
        // once this returns `Proceed` and replays each held write for
        // real; nothing needs doing here, this is just that contract's
        // anchor point.
        outcome
    }

    /// Drains every policy's reorder queue in a seeded random order, for
    /// `HardDriveImage::flush` to replay onto the image once
    /// [`Self::on_flush`] has returned `Proceed` (not `ignore`d).
    /// A flush is a barrier: once it completes, each lba must hold its
    /// newest write, even though several policies/rules each held their
    /// own queue and the guest may have written the same sector more
    /// than once before the flush landed. Dedup by lba (keeping the
    /// entry with the highest `seq`) *before* shuffling, not after --
    /// shuffling first and then keeping "whichever copy of this lba
    /// landed last in the random order" would still let an older write
    /// win by chance.
    pub fn take_release_on_flush(&mut self) -> Vec<HeldWrite> {
        let seq = self.seq;
        // `BTreeMap`, not a hash map: `into_values()` below must iterate
        // in a fixed order (ascending lba) for the shuffle that follows
        // to be reproducible -- the same determinism reasoning as
        // `stale_pending`'s own doc comment.
        let mut by_lba: std::collections::BTreeMap<u64, HeldWrite> =
            std::collections::BTreeMap::new();
        for policy in &mut self.policies {
            for rule in &mut policy.rules {
                for held in rule.state.reorder_queue.drain(..) {
                    by_lba
                        .entry(held.lba)
                        .and_modify(|existing| {
                            if held.seq >= existing.seq {
                                *existing = held.clone();
                            }
                        })
                        .or_insert(held);
                }
            }
        }
        let mut items: Vec<HeldWrite> = by_lba.into_values().collect();
        // Every rule's queue is merged into one list by this point, so
        // the shuffle draw is keyed on the target's own `seq` (not a
        // per-policy seed) -- still deterministic for a given seed and
        // guest input schedule, since `seq` itself only advances from
        // those.
        let mut i = items.len();
        while i > 1 {
            i -= 1;
            let j = (rng_for(seq, seq, 0x3000 + i as u64) as usize) % (i + 1);
            items.swap(i, j);
        }
        items
    }

    /// Writes a `reorder` window overflow evicted (see the `Action::
    /// Reorder` arm of [`Self::on_write`]'s doc comment) -- drained by
    /// the caller right after the `on_write` call that produced them, so
    /// the evicted data still lands on the real medium instead of being
    /// silently discarded.
    pub fn take_evicted(&mut self) -> Vec<HeldWrite> {
        self.pending_evicted.drain(..).collect()
    }

    /// Drains the millisecond total a `delay` rule accumulated during the
    /// most recent `on_write`/`on_read`/`on_flush` call. `HardDriveImage`
    /// itself has no notion of emulated time, so it cannot act on this --
    /// a controller with a completion-timing mechanism that can accept a
    /// variable extra delay (`ScsiDisk`'s own `post(csr, cck)`, in
    /// particular) calls this right after the I/O call that may have
    /// produced it and adds the converted-to-cck equivalent to its own
    /// completion schedule. Not every controller has an injection point
    /// this simple to add latency at (IDE/copperhf/floppy/CD's own
    /// completion timing is structured differently); `delay` on those
    /// targets still only shows up in `disk.fault.stats`' `delayed_ms`
    /// for now, same as before this method existed.
    pub fn take_delay_ms(&mut self) -> u64 {
        std::mem::take(&mut self.pending_delay_ms)
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
        let outcome = set.on_write(0, &[0u8; 512], 512, &[]);
        assert!(matches!(outcome, Outcome::DoneOk));
        assert_eq!(set.stats.dropped, 1);
        assert_eq!(set.pending_events.len(), 1);
    }

    #[test]
    fn pending_events_never_grows_past_the_cap() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([{"action": "delay", "ms": 0}])),
            1,
        )
        .unwrap();
        for lba in 0..(MAX_PENDING_EVENTS as u64 + 50) {
            set.on_write(lba, &[0u8; 512], 512, &[]);
        }
        assert_eq!(set.pending_events.len(), MAX_PENDING_EVENTS);
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
        let outcome = set.on_write(0, &data, 512, &[]);
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
        let outcome = set.on_write(0, &[0xAAu8; 512], 512, &[]);
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
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::Proceed
        ));
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::Proceed
        ));
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::DoneOk
        ));
    }

    #[test]
    fn after_gate_counts_from_installation_not_from_zero() {
        let mut set = FaultPolicySet::default();
        // Three writes already happened (an unrelated rule, so they don't
        // themselves get dropped) before the gated rule is even installed.
        set.set(
            Some("noop".into()),
            &policy(serde_json::json!([{"op": "read", "action": "delay", "ms": 0}])),
            1,
        )
        .unwrap();
        set.on_write(0, &[0u8; 512], 512, &[]);
        set.on_write(0, &[0u8; 512], 512, &[]);
        set.on_write(0, &[0u8; 512], 512, &[]);
        set.set(
            Some("gated".into()),
            &policy(serde_json::json!([
                {"op": "write", "action": "drop", "after": {"writes": 2}}
            ])),
            1,
        )
        .unwrap();
        // If the gate counted from zero, 3 prior writes would already
        // satisfy "after 2 writes" and this first write would be dropped.
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::Proceed
        ));
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::Proceed
        ));
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::DoneOk
        ));
    }

    #[test]
    fn reset_stats_does_not_undo_an_already_satisfied_after_gate() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "drop", "after": {"writes": 2}}
            ])),
            1,
        )
        .unwrap();
        set.on_write(0, &[0u8; 512], 512, &[]);
        set.on_write(0, &[0u8; 512], 512, &[]);
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::DoneOk
        ));
        set.reset_stats();
        // The catalogue's own promise: reset_stats does not affect
        // installed policies -- an already-live gate must stay live.
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::DoneOk
        ));
    }

    #[test]
    fn cut_fails_this_and_every_later_request_until_cleared() {
        let mut set = FaultPolicySet::default();
        set.set(None, &policy(serde_json::json!([{"action": "cut"}])), 1)
            .unwrap();
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
            Outcome::Fail(ErrorCode::NotReady)
        ));
        assert!(matches!(
            set.on_read(0, &mut [0u8; 512], 512),
            Outcome::Fail(ErrorCode::NotReady)
        ));
        set.clear(None);
        assert!(matches!(
            set.on_write(0, &[0u8; 512], 512, &[]),
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
                .map(|i| matches!(set.on_write(i, &[0u8; 512], 512, &[]), Outcome::DoneOk))
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
        assert!(matches!(
            set.on_write(5, &new_data, 512, &[]),
            Outcome::DoneOk
        ));
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
        set.on_write(1, &[1u8; 512], 512, &[]);
        set.on_write(2, &[2u8; 512], 512, &[]);
        assert!(matches!(set.on_flush(), Outcome::Proceed));
        let released = set.take_release_on_flush();
        assert_eq!(released.len(), 2);
    }

    #[test]
    fn flush_release_keeps_only_the_newest_write_for_a_sector_held_twice() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([{"op": "write", "action": "reorder", "window": 8}])),
            1,
        )
        .unwrap();
        set.on_write(5, &[1u8; 512], 512, &[]);
        set.on_write(5, &[2u8; 512], 512, &[]);
        assert!(matches!(set.on_flush(), Outcome::Proceed));
        let released = set.take_release_on_flush();
        assert_eq!(
            released.len(),
            1,
            "the two holds of lba 5 must collapse into one"
        );
        assert_eq!(released[0].data, vec![2u8; 512], "the newer write must win");
    }

    #[test]
    fn replacing_a_policy_by_id_releases_its_held_writes_instead_of_discarding_them() {
        let mut set = FaultPolicySet::default();
        set.set(
            Some("p".into()),
            &policy(serde_json::json!([{"op": "write", "action": "reorder", "window": 8}])),
            1,
        )
        .unwrap();
        set.on_write(5, &[0x42u8; 512], 512, &[]);
        let (_, released) = set
            .set(
                Some("p".into()),
                &policy(serde_json::json!([{"op": "write", "action": "drop"}])),
                1,
            )
            .unwrap();
        assert_eq!(
            released.len(),
            1,
            "replacing a policy by id must release its held writes, not drop them silently"
        );
        assert_eq!(released[0].lba, 5);
        assert_eq!(released[0].data, vec![0x42u8; 512]);
    }

    #[test]
    fn tear_then_reorder_holds_a_full_sector_not_a_sub_sector_buffer() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "tear", "at": "bytes", "n": 100, "continue": true},
                {"op": "write", "action": "reorder", "window": 4},
            ])),
            1,
        )
        .unwrap();
        let pre_write = vec![0x99u8; 512];
        set.on_write(5, &[0x42u8; 512], 512, &pre_write);
        assert!(matches!(set.on_flush(), Outcome::Proceed));
        let released = set.take_release_on_flush();
        assert_eq!(released.len(), 1);
        // Must be a full sector (not a 100-byte sliver): the caller's own
        // `write_sector_real` always slices `data[..SECTOR_SIZE]` when
        // replaying a held write, which panics on anything shorter.
        assert_eq!(released[0].data.len(), 512);
        assert_eq!(&released[0].data[..100], &[0x42u8; 100]);
        assert_eq!(
            &released[0].data[100..],
            &pre_write[100..],
            "the untouched tail must be spliced from the pre-write bytes"
        );
    }

    #[test]
    fn drop_then_continue_corrupt_still_drops_the_write() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "drop", "continue": true},
                {"op": "write", "action": "corrupt", "bits": 8},
            ])),
            1,
        )
        .unwrap();
        let outcome = set.on_write(0, &[0x11u8; 512], 512, &[0u8; 512]);
        assert!(
            matches!(outcome, Outcome::DoneOk),
            "drop must win even though corrupt continues after it, got {outcome:?}"
        );
        assert!(
            set.take_release_on_flush().is_empty(),
            "a dropped write must not resurface anywhere"
        );
    }

    #[test]
    fn reorder_then_continue_corrupt_mutates_the_held_copy_not_a_second_commit() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "reorder", "window": 4, "continue": true},
                {"op": "write", "action": "corrupt", "bits": 4096},
            ])),
            1,
        )
        .unwrap();
        let original = vec![0u8; 512];
        let outcome = set.on_write(0, &original, 512, &original);
        assert!(
            matches!(outcome, Outcome::DoneOk),
            "must not also produce an independent commit, got {outcome:?}"
        );
        let released = set.take_release_on_flush();
        assert_eq!(released.len(), 1, "exactly one held copy, not two");
        assert_ne!(
            released[0].data, original,
            "the held copy must carry the corruption, not an untouched one committed separately"
        );
    }

    #[test]
    fn reorder_then_continue_error_removes_the_held_write() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "reorder", "window": 4, "continue": true},
                {"op": "write", "action": "error", "code": "aborted"},
            ])),
            1,
        )
        .unwrap();
        let outcome = set.on_write(0, &[0x22u8; 512], 512, &[0u8; 512]);
        assert!(matches!(outcome, Outcome::Fail(_)));
        assert!(
            set.take_release_on_flush().is_empty(),
            "a write the guest saw fail must not survive in the held queue for a later flush \
             to resurrect"
        );
    }

    #[test]
    fn drop_after_continue_reorder_removes_the_held_write() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "reorder", "window": 4, "continue": true},
                {"op": "write", "action": "drop"},
            ])),
            1,
        )
        .unwrap();
        let outcome = set.on_write(0, &[0x33u8; 512], 512, &[0u8; 512]);
        assert!(matches!(outcome, Outcome::DoneOk));
        assert!(
            set.take_release_on_flush().is_empty(),
            "drop firing after reorder must also discard what reorder already queued"
        );
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

    #[test]
    fn corrupt_bits_near_u32_max_is_rejected_instead_of_hanging() {
        let mut set = FaultPolicySet::default();
        assert!(set
            .set(
                None,
                &policy(serde_json::json!([{"action": "corrupt", "bits": 4_000_000_000u64}])),
                1
            )
            .is_err());
        // Just over the cap, and right at it, draw the line correctly.
        assert!(set
            .set(
                None,
                &policy(serde_json::json!([{"action": "corrupt", "bits": MAX_CORRUPT_BITS + 1}])),
                1
            )
            .is_err());
        assert!(set
            .set(
                None,
                &policy(serde_json::json!([{"action": "corrupt", "bits": MAX_CORRUPT_BITS}])),
                1
            )
            .is_ok());
    }

    #[test]
    fn tear_at_sector_with_n_near_u64_max_does_not_overflow() {
        let mut set = FaultPolicySet::default();
        set.set(
            None,
            &policy(serde_json::json!([
                {"op": "write", "action": "tear", "at": "sector", "n": u64::MAX}
            ])),
            1,
        )
        .unwrap();
        // Must not panic (a plain `n as usize * sector_bytes` would
        // overflow `usize` here) and must clamp down to the whole
        // sector, same as any other `n` large enough to keep it all.
        let outcome = set.on_write(0, &[0x42u8; 512], 512, &[0u8; 512]);
        match outcome {
            Outcome::ProceedWith(data) => assert_eq!(data, vec![0x42u8; 512]),
            other => panic!("expected ProceedWith(whole sector), got {other:?}"),
        }
    }
}
