//! The SyncPlay state machine: one shared playback per group, kept by the server.
//!
//! Pure and time-explicit (every call gets `now` in server ms), so it is tested
//! without clocks or sockets.
//!
//! * Playing is `position at anchor` plus elapsed time: clients compute where they
//!   should be from their estimate of the server clock (ping/pong).
//! * Play starts slightly in the future ([`START_DELAY_MS`]) so every member starts
//!   together.
//! * A seek, a new item or a member that buffers puts the group into *waiting*: it is
//!   paused until every member reports ready (or [`WAIT_TIMEOUT_MS`] passes), then it
//!   resumes if it was playing.
//! * Members report their position while playing; [`correction`] turns the drift
//!   into a playback-rate nudge, or a seek when they are far off.

use std::collections::BTreeMap;

use serde::Serialize;
use utoipa::ToSchema;

pub type MemberId = u64;

/// How far in the future a play command starts, so all members start together.
pub const START_DELAY_MS: i64 = 500;
/// Members that are not ready after this long are no longer waited for.
pub const WAIT_TIMEOUT_MS: i64 = 15_000;
/// Drift below this is left alone.
pub const DRIFT_TOLERANCE_MS: i64 = 120;
/// Drift from this on is fixed with a seek instead of a rate change.
pub const DRIFT_SEEK_MS: i64 = 2_000;
/// Largest playback-rate nudge (±8 %, barely audible).
pub const MAX_RATE_DELTA: f64 = 0.08;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MemberState {
    pub id: MemberId,
    pub user_id: i64,
    /// Profile name.
    pub name: String,
    /// Loaded and able to play at the group position.
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    Paused {
        position_ms: i64,
    },
    /// At `position_ms` at server time `anchor_ms`, advancing in real time.
    Playing {
        position_ms: i64,
        anchor_ms: i64,
    },
}

/// A request of one member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Play,
    /// Where the member paused; the group's own position when `None`.
    Pause(Option<i64>),
    Seek(i64),
    /// The member's player is buffering.
    Buffering,
    /// The member's player can play at the group position.
    Ready,
    /// Switch everybody to another item (e.g. the next episode), from its start.
    SetItem(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError {
    /// The member is not (or no longer) in the group.
    NotAMember,
}

/// What a member should do about its drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Correction {
    InSync,
    /// Play at this rate until back in sync.
    Rate(f64),
    /// Too far off: jump to this position.
    Seek(i64),
}

/// The policy for a member that is `drift_ms` ahead (positive) or behind (negative)
/// of where the group is (`expected_ms`).
pub fn correction(drift_ms: i64, expected_ms: i64) -> Correction {
    let magnitude = drift_ms.abs();
    if magnitude <= DRIFT_TOLERANCE_MS {
        Correction::InSync
    } else if magnitude >= DRIFT_SEEK_MS {
        Correction::Seek(expected_ms.max(0))
    } else {
        // 5 % per second of drift: one second is caught up in about 20 s.
        let delta = (drift_ms as f64 / 1000.0 * 0.05).clamp(-MAX_RATE_DELTA, MAX_RATE_DELTA);
        Correction::Rate(((1.0 - delta) * 1000.0).round() / 1000.0)
    }
}

/// The shared playback of a group.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupState {
    pub item_id: i64,
    pub playback: Playback,
    /// `Some(resume)` while waiting for members; `resume` = play once all are ready.
    pub waiting: Option<bool>,
    pub waiting_since: i64,
    pub members: BTreeMap<MemberId, MemberState>,
    /// Bumped on every change; clients ignore older states.
    pub revision: u64,
}

impl GroupState {
    pub fn new(item_id: i64, position_ms: i64) -> Self {
        Self {
            item_id,
            playback: Playback::Paused {
                position_ms: position_ms.max(0),
            },
            waiting: None,
            waiting_since: 0,
            members: BTreeMap::new(),
            revision: 1,
        }
    }

    /// Where the group is at server time `now`.
    pub fn position_at(&self, now: i64) -> i64 {
        match self.playback {
            Playback::Paused { position_ms } => position_ms,
            Playback::Playing {
                position_ms,
                anchor_ms,
            } => position_ms + (now - anchor_ms).max(0),
        }
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.playback, Playback::Playing { .. })
    }

    fn changed(&mut self) -> bool {
        self.revision += 1;
        true
    }

    fn all_ready(&self) -> bool {
        self.members.values().all(|m| m.ready)
    }

    fn start_waiting(&mut self, resume: bool, now: i64) {
        let position_ms = self.position_at(now);
        self.playback = Playback::Paused { position_ms };
        self.waiting = Some(resume);
        self.waiting_since = now;
    }

    /// Ends a wait: resumes (slightly in the future) or stays paused.
    fn finish_waiting(&mut self, now: i64) {
        if let Some(resume) = self.waiting.take()
            && resume
        {
            self.playback = Playback::Playing {
                position_ms: self.position_at(now),
                anchor_ms: now + START_DELAY_MS,
            };
        }
    }

    fn finish_if_all_ready(&mut self, now: i64) {
        if self.waiting.is_some() && self.all_ready() {
            self.finish_waiting(now);
        }
    }

    /// A member joins. It counts as ready unless the group is waiting (then it has to
    /// load the item like everybody else); if its player buffers, it says so.
    pub fn join(&mut self, mut member: MemberState, _now: i64) -> bool {
        member.ready = self.waiting.is_none();
        self.members.insert(member.id, member);
        self.changed()
    }

    /// A member leaves; nobody waits for it anymore.
    pub fn leave(&mut self, id: MemberId, now: i64) -> bool {
        if self.members.remove(&id).is_none() {
            return false;
        }
        self.finish_if_all_ready(now);
        self.changed()
    }

    /// Applies a member's command. Returns whether the state changed.
    pub fn apply(&mut self, from: MemberId, command: Command, now: i64) -> Result<bool, SyncError> {
        if !self.members.contains_key(&from) {
            return Err(SyncError::NotAMember);
        }
        let changed = match command {
            Command::Play => match self.waiting {
                Some(resume) if !resume => {
                    self.waiting = Some(true);
                    self.finish_if_all_ready(now);
                    true
                }
                Some(_) => false,
                None if self.is_playing() => false,
                None if self.all_ready() => {
                    self.waiting = Some(true);
                    self.finish_waiting(now);
                    true
                }
                None => {
                    self.start_waiting(true, now);
                    true
                }
            },
            Command::Pause(at) => {
                let position_ms = at.map_or_else(|| self.position_at(now), |p| p.max(0));
                let before = (self.playback, self.waiting);
                self.playback = Playback::Paused { position_ms };
                if self.waiting.is_some() {
                    self.waiting = Some(false);
                }
                before != (self.playback, self.waiting)
            }
            Command::Seek(position_ms) => {
                let resume = self.is_playing() || self.waiting == Some(true);
                self.playback = Playback::Paused {
                    position_ms: position_ms.max(0),
                };
                for member in self.members.values_mut() {
                    member.ready = false;
                }
                self.waiting = Some(resume);
                self.waiting_since = now;
                true
            }
            Command::Buffering => {
                let member = self.members.get_mut(&from).expect("checked above");
                let was_ready = std::mem::replace(&mut member.ready, false);
                if self.is_playing() {
                    self.start_waiting(true, now);
                    true
                } else {
                    was_ready
                }
            }
            Command::Ready => {
                let member = self.members.get_mut(&from).expect("checked above");
                let was_ready = std::mem::replace(&mut member.ready, true);
                let waiting = self.waiting;
                self.finish_if_all_ready(now);
                !was_ready || waiting != self.waiting
            }
            Command::SetItem(item_id) => {
                self.item_id = item_id;
                self.playback = Playback::Paused { position_ms: 0 };
                for member in self.members.values_mut() {
                    member.ready = false;
                }
                self.waiting = Some(true);
                self.waiting_since = now;
                true
            }
        };
        Ok(changed && self.changed())
    }

    /// Periodic housekeeping: stop waiting for members that never get ready.
    pub fn tick(&mut self, now: i64) -> bool {
        if self.waiting.is_some() && now - self.waiting_since >= WAIT_TIMEOUT_MS {
            for member in self.members.values_mut() {
                member.ready = true;
            }
            self.finish_waiting(now);
            return self.changed();
        }
        false
    }

    /// What a member should do about its reported position (`position_ms` at server
    /// time `at_ms`). Only while the group plays and is not waiting.
    pub fn drift(&self, position_ms: i64, at_ms: i64) -> Option<Correction> {
        match self.playback {
            Playback::Playing { anchor_ms, .. } if self.waiting.is_none() && at_ms >= anchor_ms => {
                let expected = self.position_at(at_ms);
                Some(correction(position_ms - expected, expected))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: MemberId) -> MemberState {
        MemberState {
            id,
            user_id: id as i64,
            name: format!("Mitglied {id}"),
            ready: false,
        }
    }

    /// A group with two ready members, paused at 10 s.
    fn group() -> GroupState {
        let mut g = GroupState::new(7, 10_000);
        g.join(member(1), 0);
        g.join(member(2), 0);
        g
    }

    #[test]
    fn play_starts_together_and_pause_freezes_the_position() {
        let mut g = group();
        assert!(g.members.values().all(|m| m.ready), "joining a calm group");
        assert!(g.apply(1, Command::Play, 1_000).unwrap());
        assert_eq!(
            g.playback,
            Playback::Playing {
                position_ms: 10_000,
                anchor_ms: 1_000 + START_DELAY_MS
            }
        );
        // Before the anchor nobody moves; afterwards in real time.
        assert_eq!(g.position_at(1_200), 10_000);
        assert_eq!(g.position_at(3_500), 12_000);
        assert!(
            !g.apply(2, Command::Play, 2_000).unwrap(),
            "already playing"
        );

        let revision = g.revision;
        assert!(g.apply(2, Command::Pause(None), 4_500).unwrap());
        assert_eq!(
            g.playback,
            Playback::Paused {
                position_ms: 13_000
            }
        );
        assert!(g.revision > revision);
        assert_eq!(g.position_at(99_000), 13_000);
        // The member's own position wins (where it pressed pause).
        assert!(g.apply(1, Command::Pause(Some(12_900)), 4_600).unwrap());
        assert_eq!(g.position_at(5_000), 12_900);
        assert!(!g.apply(1, Command::Pause(Some(12_900)), 4_700).unwrap());
        assert_eq!(g.apply(9, Command::Play, 0), Err(SyncError::NotAMember));
    }

    #[test]
    fn a_buffering_member_pauses_everybody_until_it_is_ready() {
        let mut g = group();
        g.apply(1, Command::Play, 0).unwrap();
        let start = START_DELAY_MS;
        assert!(g.apply(2, Command::Buffering, start + 3_000).unwrap());
        assert_eq!(
            g.playback,
            Playback::Paused {
                position_ms: 13_000
            }
        );
        assert_eq!(g.waiting, Some(true));
        assert!(!g.members[&2].ready && g.members[&1].ready);
        // Others pressing play while waiting changes nothing yet.
        assert!(!g.apply(1, Command::Play, start + 3_500).unwrap());
        assert!(g.apply(2, Command::Ready, start + 4_000).unwrap());
        assert_eq!(g.waiting, None);
        assert_eq!(
            g.playback,
            Playback::Playing {
                position_ms: 13_000,
                anchor_ms: start + 4_000 + START_DELAY_MS
            }
        );
    }

    #[test]
    fn seeks_wait_for_all_members_and_resume_only_if_playing() {
        let mut g = group();
        g.apply(1, Command::Play, 0).unwrap();
        assert!(g.apply(1, Command::Seek(60_000), 2_000).unwrap());
        assert_eq!(
            g.playback,
            Playback::Paused {
                position_ms: 60_000
            }
        );
        assert_eq!(g.waiting, Some(true));
        assert!(g.members.values().all(|m| !m.ready));
        g.apply(1, Command::Ready, 2_200).unwrap();
        assert!(g.waiting.is_some(), "member 2 is still loading");
        g.apply(2, Command::Ready, 2_400).unwrap();
        assert_eq!(
            g.playback,
            Playback::Playing {
                position_ms: 60_000,
                anchor_ms: 2_400 + START_DELAY_MS
            }
        );

        // Paused group: a seek waits as well, but stays paused afterwards …
        let mut g = group();
        g.apply(2, Command::Seek(5_000), 100).unwrap();
        assert_eq!(g.waiting, Some(false));
        g.apply(1, Command::Ready, 200).unwrap();
        g.apply(2, Command::Ready, 300).unwrap();
        assert_eq!(
            (g.playback, g.waiting),
            (Playback::Paused { position_ms: 5_000 }, None)
        );
        // … unless someone presses play meanwhile.
        g.apply(2, Command::Seek(6_000), 400).unwrap();
        g.apply(1, Command::Play, 450).unwrap();
        assert_eq!(g.waiting, Some(true));
        g.apply(1, Command::Ready, 500).unwrap();
        g.apply(2, Command::Ready, 600).unwrap();
        assert!(g.is_playing());
        // A pause while waiting cancels the resume.
        g.apply(1, Command::Seek(7_000), 700).unwrap();
        g.apply(2, Command::Pause(None), 750).unwrap();
        assert_eq!(g.waiting, Some(false));
        g.apply(1, Command::Ready, 800).unwrap();
        g.apply(2, Command::Ready, 900).unwrap();
        assert_eq!(g.playback, Playback::Paused { position_ms: 7_000 });
    }

    #[test]
    fn nobody_waits_forever() {
        let mut g = group();
        g.apply(1, Command::Play, 0).unwrap();
        g.apply(2, Command::Buffering, 1_000).unwrap();
        assert!(!g.tick(1_000 + WAIT_TIMEOUT_MS - 1));
        assert!(g.tick(1_000 + WAIT_TIMEOUT_MS));
        assert!(g.is_playing() && g.waiting.is_none());

        // A member that leaves while the others wait for it.
        let mut g = group();
        g.apply(1, Command::Play, 0).unwrap();
        g.apply(2, Command::Buffering, 1_000).unwrap();
        assert!(g.leave(2, 1_100));
        assert!(g.is_playing());
        assert!(!g.leave(2, 1_200), "already gone");
    }

    #[test]
    fn newcomers_and_new_items() {
        let mut g = group();
        g.apply(1, Command::Seek(1_000), 0).unwrap();
        g.join(member(3), 10);
        assert!(
            !g.members[&3].ready,
            "joins a waiting group: must load first"
        );

        let mut g = group();
        g.apply(1, Command::Play, 0).unwrap();
        assert!(g.apply(2, Command::SetItem(8), 5_000).unwrap());
        assert_eq!(
            (g.item_id, g.playback, g.waiting),
            (8, Playback::Paused { position_ms: 0 }, Some(true))
        );
        g.apply(1, Command::Ready, 5_500).unwrap();
        g.apply(2, Command::Ready, 6_000).unwrap();
        assert!(
            g.is_playing(),
            "the next episode starts once everybody loaded it"
        );
    }

    #[test]
    fn drift_turns_into_rate_nudges_or_seeks() {
        assert_eq!(correction(50, 10_000), Correction::InSync);
        assert_eq!(correction(-120, 10_000), Correction::InSync);
        // 1 s ahead: slow down by 5 %; 600 ms behind: speed up by 3 %.
        assert_eq!(correction(1_000, 10_000), Correction::Rate(0.95));
        assert_eq!(correction(-600, 10_000), Correction::Rate(1.03));
        // Never more than ±8 %.
        assert_eq!(correction(1_900, 10_000), Correction::Rate(0.920));
        assert_eq!(correction(-1_999, 10_000), Correction::Rate(1.08));
        assert_eq!(correction(2_000, 10_000), Correction::Seek(10_000));
        assert_eq!(correction(-5_000, -3), Correction::Seek(0));

        let mut g = group();
        assert_eq!(g.drift(10_000, 0), None, "paused: nothing to correct");
        g.apply(1, Command::Play, 0).unwrap();
        let anchor = START_DELAY_MS;
        assert_eq!(g.drift(10_000, anchor - 1), None, "not started yet");
        assert_eq!(
            g.drift(14_000, anchor + 3_000),
            Some(Correction::Rate(0.95))
        );
        assert_eq!(g.drift(13_050, anchor + 3_000), Some(Correction::InSync));
        assert_eq!(
            g.drift(20_000, anchor + 3_000),
            Some(Correction::Seek(13_000))
        );
    }
}
