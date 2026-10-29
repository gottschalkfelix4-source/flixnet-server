//! Intro and credits detection as background work, one job per season.
//!
//! A season is analyzed after its episodes have been probed (queued with a short
//! delay, so a freshly scanned season is handled in one go) and by a periodic
//! backfill. Fingerprints are cached per file: a new episode costs one fingerprint
//! run plus the (cheap) comparison with its neighbours. ffmpeg runs happen outside
//! any write transaction; results are written in one short batch at the end.

use std::path::Path;
use std::time::Duration;

use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobQueue, Pool, priority};
use flixnet_core::scheduler::TaskDef;
use flixnet_db::segments::{self, EpisodeFile, NewSegment, StoredFingerprint};
use flixnet_db::{Db, DbError, media};
use flixnet_media::segments::{
    AnalysisError, CREDITS_END_SLACK_MS, Detection, Fingerprint, MatchConfig, Region,
    SegmentAnalyzer, Span, detect_common,
};
use serde::{Deserialize, Serialize};

/// Delay before a season queued by a probe is analyzed: the rest of the season is
/// usually being probed at the same moment.
const AFTER_PROBE_DELAY: Duration = Duration::from_secs(10);

/// Files shorter than this are not episodes worth analyzing (trailers, samples).
const MIN_DURATION_MS: i64 = 60_000;

// ---- Analyze one season -------------------------------------------------------------

pub struct AnalyzeSeason {
    pub analyzer: SegmentAnalyzer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeasonPayload {
    /// The episodes' parent item (normally a season).
    pub season_id: i64,
}

impl JobHandler for AnalyzeSeason {
    const KIND: &'static str = "media.segments";
    const POOL: Pool = Pool::Cpu;
    type Payload = SeasonPayload;

    async fn run(&self, ctx: JobContext, payload: SeasonPayload) -> anyhow::Result<()> {
        analyze_season(ctx.db(), &self.analyzer, payload.season_id).await?;
        Ok(())
    }
}

/// Queues the analysis of a season (deduplicated while queued or running).
pub async fn queue_season(
    queue: &JobQueue,
    season_id: i64,
    prio: i64,
    delay: Option<Duration>,
) -> Result<Option<i64>, DbError> {
    queue
        .enqueue::<AnalyzeSeason>(
            &SeasonPayload { season_id },
            JobOptions {
                priority: prio,
                dedupe_key: Some(format!("segments:{season_id}")),
                delay,
            },
        )
        .await
}

/// After a probe: queues the file's season if the file is an episode in a library
/// with intro detection.
pub async fn queue_after_probe(db: &Db, queue: &JobQueue, file_id: i64) -> Result<(), DbError> {
    if let Some(season_id) = segments::season_of_file(db, file_id).await? {
        queue_season(
            queue,
            season_id,
            priority::BACKFILL,
            Some(AFTER_PROBE_DELAY),
        )
        .await?;
    }
    Ok(())
}

/// What one analysis did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeasonReport {
    pub files: usize,
    pub fingerprinted: usize,
    pub intros: usize,
    pub credits: usize,
}

/// One episode file with what the analysis needs.
struct Candidate {
    file: EpisodeFile,
    duration_ms: i64,
    audio: Option<i64>,
    video: Option<i64>,
    head: Option<Fingerprint>,
    tail: Option<Fingerprint>,
}

/// Fingerprints the season's files that are not cached yet, compares them and stores
/// the detected intros and credits. Without Chromaprint it does nothing.
pub async fn analyze_season(
    db: &Db,
    analyzer: &SegmentAnalyzer,
    season_id: i64,
) -> anyhow::Result<SeasonReport> {
    let mut report = SeasonReport::default();
    if !analyzer.chromaprint_available().await {
        return Ok(report);
    }
    // Episodes probed while this runs are picked up by another round (their own
    // queue attempt was deduplicated against this job).
    for _round in 0..3 {
        let files = segments::season_files(db, season_id).await?;
        let mut candidates = Vec::with_capacity(files.len());
        let mut computed = 0;
        for file in files {
            let (candidate, fresh) = prepare(db, analyzer, file).await?;
            computed += fresh;
            candidates.push(candidate);
        }
        report.fingerprinted += computed;
        if computed == 0 && report.files > 0 {
            break; // nothing new since the last round
        }
        report.files = candidates.len();
        let (intros, credits) = store_detections(db, analyzer, &candidates).await?;
        report.intros = intros;
        report.credits = credits;
        if computed == 0 {
            break;
        }
    }
    tracing::info!(
        season = season_id,
        files = report.files,
        fingerprinted = report.fingerprinted,
        intros = report.intros,
        credits = report.credits,
        "segment analysis done"
    );
    Ok(report)
}

/// Loads (or computes and caches) both fingerprints of a file. Returns how many were
/// computed.
async fn prepare(
    db: &Db,
    analyzer: &SegmentAnalyzer,
    file: EpisodeFile,
) -> anyhow::Result<(Candidate, usize)> {
    let streams = media::streams(db, file.file_id).await?;
    let pick = |kind: &str| {
        let of_kind = || streams.iter().filter(|s| s.kind == kind && !s.is_external);
        of_kind()
            .find(|s| s.is_default)
            .or_else(|| of_kind().next())
            .map(|s| s.idx)
    };
    let mut candidate = Candidate {
        duration_ms: file.duration_ms.unwrap_or(0),
        audio: pick("audio"),
        video: pick("video"),
        head: None,
        tail: None,
        file,
    };
    let Some(audio) = candidate
        .audio
        .filter(|_| candidate.duration_ms >= MIN_DURATION_MS)
    else {
        return Ok((candidate, 0));
    };
    let mut computed = 0;
    for region in [Region::Head, Region::Tail] {
        let id = candidate.file.file_id;
        let cached = segments::fingerprint(db, id, region.as_str())
            .await?
            .filter(|f| f.stream_index == audio);
        let stored = match cached {
            Some(stored) => stored,
            None => {
                let (start, length) = region.window(candidate.duration_ms);
                let path = Path::new(&candidate.file.path);
                let items = match analyzer.fingerprint(path, audio, start, length).await {
                    Ok(print) => print.items,
                    // A broken or undecodable file: remember that, do not retry forever.
                    Err(AnalysisError::Failed(message)) => {
                        tracing::warn!(file = id, %message, "fingerprinting failed");
                        Vec::new()
                    }
                    Err(err) => return Err(err.into()),
                };
                let stored = StoredFingerprint {
                    stream_index: audio,
                    start_ms: start,
                    items,
                };
                segments::store_fingerprint(db, id, region.as_str(), &stored).await?;
                computed += 1;
                stored
            }
        };
        let print =
            (!stored.items.is_empty()).then(|| Fingerprint::new(stored.start_ms, stored.items));
        match region {
            Region::Head => candidate.head = print,
            Region::Tail => candidate.tail = print,
        }
    }
    Ok((candidate, computed))
}

/// Compares the season's fingerprints, refines the edges and replaces the stored
/// detections. Returns how many intros and credits were found.
async fn store_detections(
    db: &Db,
    analyzer: &SegmentAnalyzer,
    candidates: &[Candidate],
) -> anyhow::Result<(usize, usize)> {
    let heads: Vec<Option<Fingerprint>> = candidates.iter().map(|c| c.head.clone()).collect();
    let tails: Vec<Option<Fingerprint>> = candidates.iter().map(|c| c.tail.clone()).collect();
    // Comparing is pure CPU work: keep it off the async workers.
    let (intros, credits) = tokio::task::spawn_blocking(move || {
        fn refs(prints: &[Option<Fingerprint>]) -> Vec<Option<&Fingerprint>> {
            prints.iter().map(Option::as_ref).collect()
        }
        (
            detect_common(&refs(&heads), &MatchConfig::INTRO),
            detect_common(&refs(&tails), &MatchConfig::CREDITS),
        )
    })
    .await?;

    let mut found = Vec::new();
    for (candidate, (intro, credit)) in candidates.iter().zip(intros.into_iter().zip(credits)) {
        let path = Path::new(&candidate.file.path);
        let duration = candidate.duration_ms;
        for (kind, detection) in [("intro", intro), ("credits", credit)] {
            let Some(Detection { span, confidence }) = detection else {
                continue;
            };
            let mut span = match analyzer
                .refine_edges(path, candidate.video, candidate.audio, span)
                .await
            {
                Ok(refined) => refined,
                Err(err) => {
                    tracing::debug!(file = candidate.file.file_id, %err, "black/silence detection failed");
                    span
                }
            };
            if kind == "credits" && duration - span.end_ms <= CREDITS_END_SLACK_MS {
                span.end_ms = duration;
            }
            let span = Span {
                start_ms: span.start_ms.clamp(0, duration),
                end_ms: span.end_ms.clamp(0, duration),
            };
            if span.duration_ms() <= 0 {
                continue;
            }
            found.push(NewSegment {
                file_id: candidate.file.file_id,
                kind,
                start_ms: span.start_ms,
                end_ms: span.end_ms,
                source: "fingerprint",
                confidence,
            });
        }
    }
    let files: Vec<i64> = candidates.iter().map(|c| c.file.file_id).collect();
    segments::replace_detected(db, &files, &found).await?;
    let count = |kind: &str| found.iter().filter(|s| s.kind == kind).count();
    Ok((count("intro"), count("credits")))
}

// ---- Backfill (scheduled) -----------------------------------------------------------

pub struct AnalyzePendingSeasons;

impl JobHandler for AnalyzePendingSeasons {
    const KIND: &'static str = "media.segments_backfill";
    const POOL: Pool = Pool::Io;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        for season_id in segments::seasons_pending(ctx.db()).await? {
            queue_season(ctx.queue(), season_id, priority::BACKFILL, None).await?;
        }
        Ok(())
    }
}

/// Catches seasons whose analysis was missed (server stopped, detection switched on).
pub fn tasks() -> Vec<TaskDef> {
    vec![TaskDef::new::<AnalyzePendingSeasons>(
        "detect_segments",
        "Intros und Abspänne neuer Folgen erkennen",
        Duration::from_secs(12 * 60 * 60),
        &(),
    )]
}
