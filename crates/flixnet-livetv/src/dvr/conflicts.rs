//! Conflict detection against tuner limits (pure function).
//!
//! Every recording needs a tuner of its channel's source for its padded time span;
//! recordings of the same channel at the same time share one (one upstream per
//! channel). Recordings are admitted greedily — running ones first, then by priority
//! (higher first), then by start, then by age — and one that would need more tuners
//! than its source has at any moment is a conflict and will not be recorded. Live
//! viewers are not planned for: a recording that starts takes a tuner from them.

use std::collections::{HashMap, HashSet};

/// A recording to place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub id: i64,
    pub channel_id: i64,
    pub tuner_host_id: i64,
    /// Padded span `[start, end)`.
    pub start_ms: i64,
    pub end_ms: i64,
    pub priority: i64,
    /// Already recording: holds its tuner whatever the priorities.
    pub running: bool,
}

/// Ids of the slots that do not fit. `limits` maps tuner hosts to their tuner count
/// (missing or 0 = unlimited).
pub fn find(slots: &[Slot], limits: &HashMap<i64, i64>) -> HashSet<i64> {
    let mut order: Vec<&Slot> = slots.iter().collect();
    order.sort_by_key(|s| (!s.running, -s.priority, s.start_ms, s.id));
    let mut admitted: Vec<&Slot> = Vec::new();
    let mut conflicts = HashSet::new();
    for slot in order {
        let limit = limits.get(&slot.tuner_host_id).copied().unwrap_or(0);
        if limit <= 0 || slot.running || fits(slot, &admitted, limit) {
            admitted.push(slot);
        } else {
            conflicts.insert(slot.id);
        }
    }
    conflicts
}

/// Whether `slot` fits next to the admitted slots of its host.
fn fits(slot: &Slot, admitted: &[&Slot], limit: i64) -> bool {
    let overlapping: Vec<&&Slot> = admitted
        .iter()
        .filter(|s| {
            s.tuner_host_id == slot.tuner_host_id
                && s.start_ms < slot.end_ms
                && slot.start_ms < s.end_ms
        })
        .collect();
    // The number of channels in use only rises where a slot starts.
    let points = std::iter::once(slot.start_ms).chain(
        overlapping
            .iter()
            .map(|s| s.start_ms)
            .filter(|t| *t > slot.start_ms && *t < slot.end_ms),
    );
    for t in points {
        let mut channels: HashSet<i64> = overlapping
            .iter()
            .filter(|s| s.start_ms <= t && t < s.end_ms)
            .map(|s| s.channel_id)
            .collect();
        channels.insert(slot.channel_id);
        if channels.len() as i64 > limit {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn slot(id: i64, channel: i64, start: i64, end: i64, priority: i64) -> Slot {
        Slot {
            id,
            channel_id: channel,
            tuner_host_id: 1,
            start_ms: start * MIN,
            end_ms: end * MIN,
            priority,
            running: false,
        }
    }

    fn limits(n: i64) -> HashMap<i64, i64> {
        [(1, n)].into()
    }

    fn sorted(set: HashSet<i64>) -> Vec<i64> {
        let mut v: Vec<i64> = set.into_iter().collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn overlapping_recordings_beyond_the_tuner_count_conflict() {
        let slots = [
            slot(1, 10, 0, 60, 0),
            slot(2, 20, 30, 90, 0),
            slot(3, 30, 45, 50, 0),
            slot(4, 40, 90, 120, 0), // starts when 2 ends: fits
        ];
        assert_eq!(sorted(find(&slots, &limits(2))), [3]);
        assert_eq!(sorted(find(&slots, &limits(1))), [2, 3]);
        assert!(find(&slots, &limits(0)).is_empty(), "0 = unlimited");
        assert!(find(&slots, &HashMap::new()).is_empty());
    }

    #[test]
    fn the_same_channel_shares_a_tuner() {
        let slots = [
            slot(1, 10, 0, 60, 0),
            slot(2, 10, 55, 120, 0), // same channel, padding overlaps
            slot(3, 20, 70, 80, 0),
        ];
        assert_eq!(sorted(find(&slots, &limits(1))), [3]);
        assert!(find(&slots, &limits(2)).is_empty());
    }

    #[test]
    fn priority_decides_and_running_recordings_keep_their_tuner() {
        let slots = [slot(1, 10, 0, 60, 0), slot(2, 20, 10, 50, 5)];
        assert_eq!(
            sorted(find(&slots, &limits(1))),
            [1],
            "the later but more important one wins"
        );
        let running = [
            Slot {
                running: true,
                ..slot(1, 10, 0, 60, 0)
            },
            slot(2, 20, 10, 50, 5),
        ];
        assert_eq!(sorted(find(&running, &limits(1))), [2]);
    }

    #[test]
    fn hosts_are_counted_separately() {
        let slots = [
            slot(1, 10, 0, 60, 0),
            Slot {
                tuner_host_id: 2,
                ..slot(2, 20, 0, 60, 0)
            },
        ];
        let limits: HashMap<i64, i64> = [(1, 1), (2, 1)].into();
        assert!(find(&slots, &limits).is_empty());
    }

    #[test]
    fn a_conflict_is_detected_where_it_happens_not_at_the_start() {
        // 1 and 3 are admitted first (priority); 4 fits at its start but not once 3
        // starts in the middle of it. 2 ended before 4 begins.
        let slots = [
            slot(1, 10, 0, 100, 1),
            slot(2, 20, 0, 30, 1),
            slot(3, 30, 50, 100, 1),
            slot(4, 40, 40, 70, 0),
        ];
        assert_eq!(sorted(find(&slots, &limits(2))), [4]);
        assert!(find(&slots, &limits(3)).is_empty());
    }
}
