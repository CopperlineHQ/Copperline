// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded, frame-indexed input prediction and rollback, independent of transport.
//!
//! Every player owns one controller port. Each frame executes one input per
//! port, so two players and a four-player adapter session differ only in how
//! many ports a frame carries: prediction, confirmation and replay are the
//! same for all of them.

use super::{Input, MAX_PLAYERS};
use anyhow::{ensure, Result};
use std::collections::{BTreeMap, VecDeque};

const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
pub(super) const HASH_INTERVAL: u64 = 60;

pub(super) trait Machine {
    fn save(&self) -> Result<Vec<u8>>;
    fn load(&mut self, state: &[u8]) -> Result<()>;
    /// One frame with one input per controller port, in port order.
    fn frame(&mut self, inputs: &[Input], previous_keys: [u8; 16], replay: bool) -> Result<()>;
}

struct Frame {
    number: u64,
    before: Vec<u8>,
    /// The complete input set this frame executed, remote predictions included.
    inputs: [Input; MAX_PLAYERS],
    previous_keys: [u8; 16],
}

/// Confirmed frames and checkpoint digests, retained for a spectator feed
/// once every port's input for a frame is final.
#[derive(Default)]
pub(super) struct ConfirmedLog {
    pub inputs: VecDeque<(u64, [Input; MAX_PLAYERS])>,
    pub hashes: VecDeque<(u64, [u8; 32])>,
}

pub(super) struct Rollback {
    pub current: u64,
    pub confirmed: u64,
    /// Next input needed from each player; the local port's entry is unused.
    pub received: [u64; MAX_PLAYERS],
    /// What each peer has acknowledged of this peer's own input.
    pub acknowledged: [u64; MAX_PLAYERS],
    pub local: BTreeMap<u64, Input>,
    remote: [BTreeMap<u64, Input>; MAX_PLAYERS],
    /// A host relays every player's input to every other player, so it keeps
    /// each port's inputs until the last link has acknowledged them. Guests
    /// relay nothing and leave this at the confirmed frontier.
    pub relay_floor: [u64; MAX_PLAYERS],
    history: VecDeque<Frame>,
    previous_keys: [u8; 16],
    dirty: Option<u64>,
    bytes: usize,
    player: usize,
    players: usize,
    delay: u64,
    window: u64,
    pub hashes: BTreeMap<u64, [u8; 32]>,
    pub rollbacks: u64,
    pub replayed_frames: u64,
    pub log: Option<ConfirmedLog>,
}

impl Rollback {
    pub fn new(player: usize, players: usize, delay: u8, window: u8) -> Self {
        let neutral: BTreeMap<_, _> = (0..u64::from(delay))
            .map(|f| (f, Input::default()))
            .collect();
        Self {
            current: 0,
            confirmed: 0,
            received: [u64::from(delay); MAX_PLAYERS],
            acknowledged: [u64::from(delay); MAX_PLAYERS],
            local: neutral.clone(),
            remote: std::array::from_fn(|_| neutral.clone()),
            relay_floor: [u64::MAX; MAX_PLAYERS],
            history: VecDeque::new(),
            previous_keys: [0; 16],
            dirty: None,
            bytes: 0,
            player,
            players,
            delay: u64::from(delay),
            window: u64::from(window),
            hashes: BTreeMap::new(),
            rollbacks: 0,
            replayed_frames: 0,
            log: None,
        }
    }

    /// Ports owned by the other players.
    fn peers(&self) -> impl Iterator<Item = usize> + use<> {
        let (player, players) = (self.player, self.players);
        (0..players).filter(move |p| *p != player)
    }

    /// The first frame any peer's input is still missing for.
    pub fn received_frontier(&self) -> u64 {
        self.peers().map(|p| self.received[p]).min().unwrap_or(0)
    }

    /// The first local frame a peer this port sends to has yet to
    /// acknowledge. A guest submits its input to the host alone, which then
    /// owes it to the other players (see `relay_floor`), so the host's
    /// acknowledgement is the only one that can release local history.
    pub fn ack_frontier(&self) -> u64 {
        if self.player == 0 {
            self.peers()
                .map(|p| self.acknowledged[p])
                .min()
                .unwrap_or(0)
        } else {
            self.acknowledged[0]
        }
    }

    pub fn receive(&mut self, player: usize, frame: u64, input: Input) -> Result<()> {
        ensure!(
            player < self.players && player != self.player,
            "netplay input names another player's port"
        );
        // Our submitted input reaches current + delay, allowing the peer's
        // frontier to reach current + delay + 1. It can then predict `window`
        // frames and sample one more delayed input while stalled there.
        ensure!(
            frame <= self.current + self.window + 2 * self.delay + 1,
            "netplay input is too far in the future"
        );
        if let Some(old) = self.remote[player].get(&frame) {
            ensure!(
                *old == input,
                "peer changed previously submitted input at frame {frame}"
            );
            return Ok(());
        }
        if frame < self.confirmed {
            return Ok(());
        }
        self.remote[player].insert(frame, input);
        if self
            .history
            .iter()
            .any(|f| f.number == frame && f.inputs[player] != input)
        {
            self.dirty = Some(self.dirty.map_or(frame, |old| old.min(frame)));
        }
        while self.remote[player].contains_key(&self.received[player]) {
            self.received[player] += 1;
        }
        Ok(())
    }

    pub fn acknowledge(&mut self, player: usize, next: u64) -> Result<()> {
        ensure!(
            player < self.players && player != self.player,
            "netplay acknowledgement names another player's port"
        );
        let sent_end = self
            .local
            .last_key_value()
            .map_or(self.acknowledged[player], |(f, _)| f + 1);
        ensure!(
            next <= sent_end.max(self.acknowledged[player]),
            "peer acknowledged input that was never submitted"
        );
        self.acknowledged[player] = self.acknowledged[player].max(next);
        Ok(())
    }

    /// Inputs still needed by a peer that acknowledged `from` for `player`,
    /// oldest first. A host reads this to relay one port to one link.
    pub fn pending(
        &self,
        player: usize,
        from: u64,
    ) -> impl Iterator<Item = (u64, Input)> + use<'_> {
        let held = if player == self.player {
            &self.local
        } else {
            &self.remote[player]
        };
        held.range(from..).map(|(&f, &i)| (f, i))
    }

    /// The prediction for `player` at `number`: its last known input, without
    /// repeating relative mouse motion that belonged to an earlier frame.
    fn predict(&self, player: usize, number: u64) -> Input {
        self.remote[player].range(..=number).next_back().map_or(
            Input::default(),
            |(&frame, &input)| {
                if frame == number {
                    input
                } else {
                    input.without_motion()
                }
            },
        )
    }

    fn simulate(&mut self, machine: &mut impl Machine, number: u64, replay: bool) -> Result<()> {
        let local = *self
            .local
            .get(&number)
            .expect("local input exists before emulation");
        let mut inputs = [Input::default(); MAX_PLAYERS];
        for player in 0..self.players {
            inputs[player] = if player == self.player {
                local
            } else {
                self.predict(player, number)
            };
        }
        let before = machine.save()?;
        ensure!(self.bytes + before.len() <= MEMORY_LIMIT, "netplay snapshots exceed the 256 MiB memory budget; use less RAM or a smaller rollback window");
        machine.frame(&inputs[..self.players], self.previous_keys, replay)?;
        self.history.push_back(Frame {
            number,
            before,
            inputs,
            previous_keys: self.previous_keys,
        });
        self.bytes += self.history.back().unwrap().before.len();
        self.previous_keys = Input::merged_keys(&inputs[..self.players]);
        Ok(())
    }

    pub fn reconcile(&mut self, machine: &mut impl Machine) -> Result<()> {
        if let Some(first) = self.dirty.take() {
            let index = self
                .history
                .iter()
                .position(|f| f.number == first)
                .expect("unconfirmed frame retained");
            machine.load(&self.history[index].before)?;
            self.previous_keys = self.history[index].previous_keys;
            while self.history.len() > index {
                self.bytes -= self.history.pop_back().unwrap().before.len();
            }
            for frame in first..self.current {
                self.simulate(machine, frame, true)?;
            }
            self.rollbacks += 1;
            self.replayed_frames += self.current - first;
        }
        self.confirm(machine)
    }

    fn confirm(&mut self, machine: &impl Machine) -> Result<()> {
        let end = self.current.min(self.received_frontier());
        let mut checkpoint = (self.confirmed / HASH_INTERVAL + 1) * HASH_INTERVAL;
        while checkpoint <= end {
            let digest = if checkpoint == self.current {
                super::digest(&machine.save()?)
            } else {
                let state = &self
                    .history
                    .iter()
                    .find(|f| f.number == checkpoint)
                    .expect("checkpoint retained")
                    .before;
                super::digest(state)
            };
            self.hashes.insert(checkpoint, digest);
            if let Some(log) = &mut self.log {
                log.hashes.push_back((checkpoint, digest));
            }
            checkpoint += HASH_INTERVAL;
        }
        // Every port's input for a frame below `end` is final: a peer cannot
        // change a received input and nothing below `confirmed` is replayed.
        // Record them before the prunes below release them.
        if let Some(log) = &mut self.log {
            for frame in self.confirmed..end {
                let mut inputs = [Input::default(); MAX_PLAYERS];
                for (player, slot) in inputs.iter_mut().enumerate().take(self.players) {
                    let held = if player == self.player {
                        &self.local
                    } else {
                        &self.remote[player]
                    };
                    *slot = *held.get(&frame).expect("confirmed input retained");
                }
                log.inputs.push_back((frame, inputs));
            }
        }
        self.confirmed = end;
        while self.history.front().is_some_and(|f| f.number < end) {
            self.bytes -= self.history.pop_front().unwrap().before.len();
        }
        // Keep one remote input as the seed for repeat-last prediction, and
        // whatever a relaying host still owes another link.
        for player in 0..MAX_PLAYERS {
            let floor = end.min(self.relay_floor[player]).saturating_sub(1);
            self.remote[player].retain(|f, _| *f >= floor);
        }
        let local_floor = end
            .min(self.ack_frontier())
            .min(self.relay_floor[self.player]);
        self.local.retain(|f, _| *f >= local_floor);
        while self.hashes.len() > 8 {
            self.hashes.pop_first();
        }
        Ok(())
    }

    pub fn submit_local(&mut self, input: Input) -> bool {
        if let std::collections::btree_map::Entry::Vacant(entry) =
            self.local.entry(self.current + self.delay)
        {
            entry.insert(input);
            true
        } else {
            false
        }
    }

    pub fn advance(&mut self, machine: &mut impl Machine, input: Input) -> Result<bool> {
        self.submit_local(input);
        if self.current >= self.received_frontier() + self.window
            || self.current >= self.ack_frontier() + self.window
        {
            return Ok(false);
        }
        self.simulate(machine, self.current, false)?;
        self.current += 1;
        self.confirm(machine)?;
        Ok(true)
    }
}
