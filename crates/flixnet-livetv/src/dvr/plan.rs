//! The planner: turns rules and the guide into scheduled recordings and marks the
//! ones no tuner is left for. Runs after guide refreshes, rule changes and every few
//! minutes. Reads first, then writes in batches.

use std::collections::{HashMap, HashSet};

use flixnet_db::dvr::{self, NewRecording, RecordingRule};
use flixnet_db::livetv::{self, Channel, Program};
use flixnet_db::{Db, DbError, now_ms};

use super::conflicts::{self, Slot};
use super::rules::{self, Candidate, RuleFilter, RuleKind};

/// How far ahead recordings are scheduled.
pub const HORIZON_MS: i64 = 14 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanReport {
    pub scheduled: u64,
    pub removed: u64,
    pub conflicts: usize,
}

pub fn filter_of(rule: &RecordingRule) -> RuleFilter {
    RuleFilter {
        kind: RuleKind::parse(&rule.kind).unwrap_or(RuleKind::Series),
        title: rule.title.clone(),
        series_id: rule.series_id.clone(),
        channel_id: rule.channel_id,
        program_start_ms: rule.program_start_ms,
        new_only: rule.new_only,
        window: rule.window_start_min.zip(rule.window_end_min),
        utc_offset_min: rule.utc_offset_min,
    }
}

pub fn candidate(p: &Program) -> Candidate {
    Candidate {
        channel_id: p.channel,
        start_ms: p.start_ms,
        end_ms: p.end_ms,
        title: p.title.clone(),
        subtitle: p.subtitle.clone(),
        season: p.season,
        episode: p.episode,
        series_id: p.series_id.clone(),
        is_new: p.is_new,
    }
}

/// The recording a rule makes of a program.
pub fn new_recording(rule: &RecordingRule, program: &Program, channel: &Channel) -> NewRecording {
    NewRecording {
        rule_id: rule.id,
        channel_id: channel.id,
        channel_name: channel.name.clone(),
        title: program.title.clone(),
        subtitle: program.subtitle.clone(),
        description: program.description.clone(),
        categories: program.category_list(),
        season: program.season,
        episode: program.episode,
        year: program.year,
        rating: program.rating.clone(),
        series_id: program.series_id.clone(),
        is_new: program.is_new,
        icon_url: program.icon_url.clone(),
        start_ms: program.start_ms,
        end_ms: program.end_ms,
        padding_before_s: rule.padding_before_s,
        padding_after_s: rule.padding_after_s,
        priority: rule.priority,
        utc_offset_min: rule.utc_offset_min,
    }
}

/// Schedules what the rules want from the guide and recomputes conflicts.
pub async fn plan(db: &Db) -> Result<PlanReport, DbError> {
    let now = now_ms();
    let channels = livetv::list_channels(db, true).await?;
    let by_id: HashMap<i64, &Channel> = channels.iter().map(|c| (c.id, c)).collect();
    let ids: Vec<i64> = channels.iter().map(|c| c.id).collect();
    let programs: Vec<Program> = livetv::programs_between(db, &ids, now, now + HORIZON_MS)
        .await?
        .into_iter()
        // Only what has not begun yet (a running program is recorded only when the
        // user asks for it explicitly).
        .filter(|p| p.start_ms > now)
        .collect();
    let candidates: Vec<Candidate> = programs.iter().map(candidate).collect();
    let index: HashMap<(i64, i64), &Program> = programs
        .iter()
        .map(|p| ((p.channel, p.start_ms), p))
        .collect();

    let mut report = PlanReport::default();
    for rule in dvr::list_rules(db).await? {
        let filter = filter_of(&rule);
        if !rule.enabled {
            if filter.kind == RuleKind::Series {
                report.removed += dvr::delete_unwanted(db, rule.id, &[], now).await?;
            }
            continue;
        }
        let known: HashSet<String> = dvr::rule_episodes(db, rule.id)
            .await?
            .into_iter()
            .filter_map(|(s, e, t)| rules::episode_key(s, e, t.as_deref()))
            .collect();
        let wanted = rules::select(&filter, &candidates, &known);
        let recordings: Vec<NewRecording> = wanted
            .iter()
            .filter_map(|c| {
                let program = index.get(&(c.channel_id, c.start_ms))?;
                let channel = by_id.get(&c.channel_id)?;
                Some(new_recording(&rule, program, channel))
            })
            .collect();
        report.scheduled += dvr::upsert_scheduled(db, &recordings).await?;
        if filter.kind == RuleKind::Series {
            let keep: Vec<(i64, i64)> = recordings
                .iter()
                .map(|r| (r.channel_id, r.start_ms))
                .collect();
            report.removed += dvr::delete_unwanted(db, rule.id, &keep, now).await?;
        }
    }
    report.conflicts = update_conflicts(db).await?;
    if report.scheduled + report.removed > 0 || report.conflicts > 0 {
        tracing::info!(
            scheduled = report.scheduled,
            removed = report.removed,
            conflicts = report.conflicts,
            "recordings planned"
        );
    }
    Ok(report)
}

/// Recomputes which scheduled recordings do not fit the tuners. Returns how many.
pub async fn update_conflicts(db: &Db) -> Result<usize, DbError> {
    let now = now_ms();
    let hosts = livetv::list_hosts(db).await?;
    let limits: HashMap<i64, i64> = hosts.iter().map(|h| (h.id, h.tuner_count)).collect();
    let channels = livetv::list_channels(db, false).await?;
    let host_of: HashMap<i64, i64> = channels.iter().map(|c| (c.id, c.tuner_host_id)).collect();
    let slots: Vec<Slot> = dvr::upcoming(db, now)
        .await?
        .iter()
        .filter_map(|r| {
            let channel = r.channel_id?;
            let (start_ms, end_ms) = r.record_span();
            Some(Slot {
                id: r.id,
                channel_id: channel,
                tuner_host_id: *host_of.get(&channel)?,
                start_ms,
                end_ms,
                priority: r.priority,
                running: r.status == "recording",
            })
        })
        .collect();
    let mut conflicted: Vec<i64> = conflicts::find(&slots, &limits).into_iter().collect();
    conflicted.sort_unstable();
    dvr::set_conflicts(db, &conflicted).await?;
    Ok(conflicted.len())
}
