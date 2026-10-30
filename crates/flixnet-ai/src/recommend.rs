//! Recommendations ("Für dich"), "Weil du X gesehen hast" rows, "Mehr wie das",
//! onboarding and the group mode "Was schauen wir heute?".
//!
//! * A profile's taste is a handful of clusters (weighted spherical k-means over the
//!   vectors of what it watched, liked or picked during onboarding), each signal
//!   weighted by how strongly it counts and decayed with a 180-day half-life.
//! * A candidate scores by its best-matching cluster, plus a co-watch signal: what
//!   household profiles with an overlapping history watched.
//! * The final list is diversified with MMR (λ = 0.7); every pick carries a rule-based
//!   explanation ("Weil du „X“ gesehen hast").
//! * Candidates are always limited to what the viewer (for groups: every member) may
//!   see — libraries and parental controls.

use std::collections::{HashMap, HashSet};

use flixnet_db::ai::ViewingRow;
use flixnet_db::browse::{self, ItemFilter, Restriction, Sort};

use crate::Ai;
use crate::index::{BruteForceIndex, VectorIndex};
use crate::math::{self, Cluster, HALF_LIFE_MS, MMR_LAMBDA, decay, dot, kmeans, mmr};

/// How much the household's co-watch signal adds to the taste similarity.
pub const COWATCH_WEIGHT: f64 = 0.12;
/// Candidates considered for MMR after scoring.
const POOL: usize = 150;

/// Who recommendations are for: a profile with what it may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub user_id: i64,
    pub name: String,
    pub library_ids: Vec<i64>,
    pub restriction: Restriction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// Watched to the end (at least once).
    Watched,
    /// Started, not finished.
    Started,
    Favorite,
    /// Picked during onboarding.
    Seed,
}

/// How much one title tells about a profile's taste.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// Movie or series.
    pub item_id: i64,
    /// After time decay.
    pub weight: f64,
    pub at: i64,
    pub kind: SignalKind,
}

/// Aggregates viewing rows (episodes per series) and onboarding picks into weighted,
/// time-decayed signals, strongest first.
pub fn signals_from(rows: &[ViewingRow], seeds: &[(i64, i64)], now: i64) -> Vec<Signal> {
    struct Acc {
        sum: f64,
        at: i64,
        favorite: bool,
        played: bool,
    }
    let mut by_item: HashMap<i64, Acc> = HashMap::new();
    for row in rows {
        let fraction = row.watched_fraction.clamp(0.0, 1.0);
        let mut base = if row.played {
            1.0 + 0.25 * (row.play_count - 1).clamp(0, 2) as f64
        } else if row.position_ms > 0 || fraction >= 0.25 {
            0.3 + 0.5 * fraction
        } else if fraction > 0.0 {
            0.1
        } else {
            0.0
        };
        if row.favorite {
            base += 1.0;
        }
        // Explicit ratings (0–10): a bad rating cancels the signal.
        if let Some(rating) = row.rating {
            if rating < 4.0 {
                base = 0.0;
            } else {
                base *= 0.5 + rating / 10.0;
            }
        }
        if base <= 0.0 {
            continue;
        }
        let acc = by_item.entry(row.item_id).or_insert(Acc {
            sum: 0.0,
            at: 0,
            favorite: false,
            played: false,
        });
        acc.sum += base;
        acc.at = acc.at.max(row.at);
        acc.favorite |= row.favorite;
        acc.played |= row.played;
    }
    let mut out: Vec<Signal> = by_item
        .into_iter()
        .map(|(item_id, acc)| {
            // A series with 40 watched episodes counts more than one film, but not 40×.
            let strength = if acc.sum <= 1.0 {
                acc.sum
            } else {
                1.0 + acc.sum.ln()
            };
            Signal {
                item_id,
                weight: strength * decay(now - acc.at, HALF_LIFE_MS),
                at: acc.at,
                kind: if acc.favorite {
                    SignalKind::Favorite
                } else if acc.played {
                    SignalKind::Watched
                } else {
                    SignalKind::Started
                },
            }
        })
        .collect();
    let known: HashSet<i64> = out.iter().map(|s| s.item_id).collect();
    for (item_id, at) in seeds {
        if !known.contains(item_id) {
            out.push(Signal {
                item_id: *item_id,
                weight: decay(now - at, HALF_LIFE_MS),
                at: *at,
                kind: SignalKind::Seed,
            });
        }
    }
    out.retain(|s| s.weight >= 0.01);
    out.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then(a.item_id.cmp(&b.item_id))
    });
    out
}

/// Number of taste clusters for `n` signals with vectors: 1 for a few, up to 5.
pub fn cluster_count(n: usize) -> usize {
    if n < 4 {
        1
    } else {
        ((n as f64 / 2.0).sqrt().round() as usize).clamp(1, 5)
    }
}

/// A profile's taste.
#[derive(Debug, Clone, Default)]
pub struct TasteProfile {
    pub signals: Vec<Signal>,
    /// Clusters over the signals with vectors (`members` index into `signals`).
    pub clusters: Vec<Cluster>,
    /// Genre affinity: genre id → weight (unit length over all genres).
    pub genres: HashMap<i64, f64>,
}

/// Genre ids per movie/series.
pub type GenreMap = HashMap<i64, Vec<i64>>;

/// How much matching the profile's favourite genres adds to the vector similarity.
pub const GENRE_WEIGHT: f64 = 0.2;

impl TasteProfile {
    pub fn build(signals: Vec<Signal>, index: &dyn VectorIndex) -> Self {
        let with_vectors: Vec<usize> = (0..signals.len())
            .filter(|i| index.vector(signals[*i].item_id).is_some())
            .collect();
        let vectors: Vec<&[f32]> = with_vectors
            .iter()
            .filter_map(|i| index.vector(signals[*i].item_id))
            .collect();
        let weights: Vec<f64> = with_vectors.iter().map(|i| signals[*i].weight).collect();
        let mut clusters = kmeans(&vectors, &weights, cluster_count(vectors.len()), 25);
        for cluster in &mut clusters {
            cluster.members = cluster.members.iter().map(|m| with_vectors[*m]).collect();
        }
        Self {
            signals,
            clusters,
            genres: HashMap::new(),
        }
    }

    /// Adds the genre affinity: signal weights summed per genre, normalized.
    pub fn with_genres(mut self, genres_of: &GenreMap) -> Self {
        let mut weights: HashMap<i64, f64> = HashMap::new();
        for signal in &self.signals {
            for genre in genres_of.get(&signal.item_id).into_iter().flatten() {
                *weights.entry(*genre).or_default() += signal.weight;
            }
        }
        let norm = weights.values().map(|w| w * w).sum::<f64>().sqrt();
        if norm > 0.0 {
            weights.values_mut().for_each(|w| *w /= norm);
        }
        self.genres = weights;
        self
    }

    /// Cosine between the genre affinity and an item's genres (0–1).
    pub fn genre_affinity(&self, genres: &[i64]) -> f64 {
        if genres.is_empty() || self.genres.is_empty() {
            return 0.0;
        }
        let sum: f64 = genres.iter().filter_map(|g| self.genres.get(g)).sum();
        sum / (genres.len() as f64).sqrt()
    }

    /// Vector similarity plus genre affinity for one candidate.
    pub fn item_score(&self, item_id: i64, v: &[f32], genres_of: &GenreMap) -> f64 {
        let genres = genres_of
            .get(&item_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        self.score(v) + GENRE_WEIGHT * self.genre_affinity(genres)
    }

    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }

    /// Similarity of a vector to this taste: the best cluster, heavier clusters a bit
    /// preferred (a side interest still counts).
    pub fn score(&self, v: &[f32]) -> f64 {
        let max_weight = self.clusters.iter().map(|c| c.weight).fold(0.0, f64::max);
        self.clusters
            .iter()
            .map(|c| {
                let share = if max_weight > 0.0 {
                    c.weight / max_weight
                } else {
                    1.0
                };
                dot(v, &c.centroid) as f64 * (0.8 + 0.2 * share)
            })
            .fold(f64::MIN, f64::max)
    }

    /// The signal that explains a candidate best: highest similarity, nudged by weight.
    pub fn closest_signal(&self, v: &[f32], index: &dyn VectorIndex) -> Option<&Signal> {
        self.signals
            .iter()
            .filter_map(|s| {
                index
                    .vector(s.item_id)
                    .map(|sv| (s, dot(v, sv) as f64 + 0.02 * s.weight.min(2.0)))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(s, _)| s)
    }

    pub fn item_ids(&self) -> HashSet<i64> {
        self.signals.iter().map(|s| s.item_id).collect()
    }
}

/// Co-watch: for each title, how much profiles with an overlapping history watched
/// it — `Σ sim(me, other) · [other watched it] / Σ sim`, with Ochiai overlap as `sim`.
pub fn cowatch_scores(user_id: i64, watched: &[(i64, i64)]) -> HashMap<i64, f64> {
    let mut sets: HashMap<i64, HashSet<i64>> = HashMap::new();
    for (user, item) in watched {
        sets.entry(*user).or_default().insert(*item);
    }
    let Some(mine) = sets.get(&user_id) else {
        return HashMap::new();
    };
    let mut scores: HashMap<i64, f64> = HashMap::new();
    let mut total = 0.0;
    for (other, theirs) in &sets {
        if *other == user_id || theirs.is_empty() {
            continue;
        }
        let overlap = mine.intersection(theirs).count();
        if overlap == 0 {
            continue;
        }
        let sim = overlap as f64 / ((mine.len() * theirs.len()) as f64).sqrt();
        total += sim;
        for item in theirs.difference(mine) {
            *scores.entry(*item).or_default() += sim;
        }
    }
    if total > 0.0 {
        scores.values_mut().for_each(|s| *s /= total);
    }
    scores
}

/// The strictest combination of several profiles' parental controls: the lowest age
/// limit, unrated content only if every limited profile allows it, all blocked genres.
pub fn strictest(restrictions: &[Restriction]) -> Restriction {
    let max_age = restrictions.iter().filter_map(|r| r.max_age).min();
    let allow_unrated = restrictions
        .iter()
        .filter(|r| r.max_age.is_some())
        .all(|r| r.allow_unrated);
    let mut blocked: Vec<String> = Vec::new();
    for genre in restrictions.iter().flat_map(|r| &r.blocked_genres) {
        if !blocked.iter().any(|g| g.eq_ignore_ascii_case(genre)) {
            blocked.push(genre.clone());
        }
    }
    Restriction {
        max_age,
        allow_unrated: max_age.is_some() && allow_unrated,
        blocked_genres: blocked,
    }
}

/// One recommended title.
#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    pub item_id: i64,
    pub score: f64,
    /// German, rule-based: "Weil du „Alien“ gesehen hast".
    pub reason: String,
    /// The title the reason refers to.
    pub because_item_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecommendationList {
    pub items: Vec<Recommendation>,
    /// No history and no onboarding picks yet.
    pub needs_onboarding: bool,
    /// Vectors exist (AI on, items embedded).
    pub available: bool,
}

/// Visible movies and series of a viewer.
async fn candidates(
    ai: &Ai,
    library_ids: &[i64],
    restriction: &Restriction,
) -> anyhow::Result<Vec<i64>> {
    let mut filter = ItemFilter {
        library_ids: library_ids.to_vec(),
        kinds: vec!["movie".into(), "series".into()],
        ..Default::default()
    };
    filter.restrict(restriction);
    Ok(browse::visible_ids(ai.db(), &filter).await?)
}

async fn titles(
    ai: &Ai,
    ids: impl IntoIterator<Item = i64>,
) -> anyhow::Result<HashMap<i64, String>> {
    let ids: Vec<i64> = ids.into_iter().collect();
    Ok(flixnet_db::ai::titles(ai.db(), &ids)
        .await?
        .into_iter()
        .collect())
}

/// A profile's taste from its history and onboarding picks.
pub async fn profile(
    ai: &Ai,
    user_id: i64,
    index: &dyn VectorIndex,
    genres_of: &GenreMap,
) -> anyhow::Result<TasteProfile> {
    let rows = flixnet_db::ai::viewing(ai.db(), user_id).await?;
    let seeds = flixnet_db::ai::taste_seeds(ai.db(), user_id).await?;
    Ok(
        TasteProfile::build(signals_from(&rows, &seeds, flixnet_db::now_ms()), index)
            .with_genres(genres_of),
    )
}

/// Genres of every movie and series.
pub async fn genre_map(ai: &Ai) -> anyhow::Result<GenreMap> {
    let mut map: GenreMap = HashMap::new();
    for (item, genre) in flixnet_db::ai::item_genre_ids(ai.db()).await? {
        map.entry(item).or_default().push(genre);
    }
    Ok(map)
}

/// Scores candidates and picks a diverse top list with MMR: (candidate, relevance).
pub fn rank(
    candidates: &[i64],
    index: &dyn VectorIndex,
    score: impl Fn(i64, &[f32]) -> f64,
    limit: usize,
    lambda: f64,
) -> Vec<(i64, f64)> {
    let mut scored: Vec<(i64, f64)> = candidates
        .iter()
        .filter_map(|id| index.vector(*id).map(|v| (*id, score(*id, v))))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(POOL);
    if scored.is_empty() {
        return Vec::new();
    }
    let (lo, hi) = scored
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), (_, s)| {
            (lo.min(*s), hi.max(*s))
        });
    let span = (hi - lo).max(1e-9);
    let relevance: Vec<f64> = scored.iter().map(|(_, s)| (s - lo) / span).collect();
    let vectors: Vec<&[f32]> = scored
        .iter()
        .map(|(id, _)| index.vector(*id).unwrap_or_default())
        .collect();
    mmr(&relevance, limit, lambda, |i, j| {
        dot(vectors[i], vectors[j]) as f64
    })
    .into_iter()
    .map(|i| scored[i])
    .collect()
}

fn reason_for(signal: &Signal, title: &str) -> String {
    match signal.kind {
        SignalKind::Favorite | SignalKind::Seed => format!("Weil du „{title}“ magst"),
        SignalKind::Watched => format!("Weil du „{title}“ gesehen hast"),
        SignalKind::Started => format!("Weil du „{title}“ angefangen hast"),
    }
}

/// "Für dich": personal recommendations for one profile.
pub async fn for_viewer(
    ai: &Ai,
    viewer: &Viewer,
    limit: usize,
) -> anyhow::Result<RecommendationList> {
    if !ai.ensure_index().await? {
        return Ok(RecommendationList::default());
    }
    let loaded = ai.index().await;
    let Some(loaded) = loaded.as_ref() else {
        return Ok(RecommendationList::default());
    };
    let index = &loaded.index;
    let genres = genre_map(ai).await?;
    let taste = profile(ai, viewer.user_id, index, &genres).await?;
    if taste.is_empty() {
        return Ok(RecommendationList {
            items: Vec::new(),
            needs_onboarding: true,
            available: true,
        });
    }
    let watched = flixnet_db::ai::household_watched(ai.db()).await?;
    let cowatch = cowatch_scores(viewer.user_id, &watched);
    let seen = taste.item_ids();
    let pool: Vec<i64> = candidates(ai, &viewer.library_ids, &viewer.restriction)
        .await?
        .into_iter()
        .filter(|id| !seen.contains(id))
        .collect();
    let picks = rank(
        &pool,
        index,
        |id, v| {
            taste.item_score(id, v, &genres)
                + COWATCH_WEIGHT * cowatch.get(&id).copied().unwrap_or(0.0)
        },
        limit,
        MMR_LAMBDA,
    );
    let names = titles(ai, taste.signals.iter().map(|s| s.item_id)).await?;
    let items = picks
        .into_iter()
        .map(|(item_id, score)| {
            let v = index.vector(item_id).unwrap_or_default();
            let household = cowatch.get(&item_id).copied().unwrap_or(0.0);
            let closest = taste.closest_signal(v, index);
            // Mostly carried by the household: say so instead of a weak "because".
            let weak = closest
                .and_then(|s| index.vector(s.item_id))
                .is_none_or(|sv| dot(v, sv) < 0.3);
            match closest.and_then(|s| names.get(&s.item_id).map(|t| (s, t))) {
                Some((signal, title)) if !(weak && household >= 0.5) => Recommendation {
                    item_id,
                    score,
                    reason: reason_for(signal, title),
                    because_item_id: Some(signal.item_id),
                },
                _ => Recommendation {
                    item_id,
                    score,
                    reason: "Im Haushalt beliebt".to_owned(),
                    because_item_id: None,
                },
            }
        })
        .collect();
    Ok(RecommendationList {
        items,
        needs_onboarding: false,
        available: true,
    })
}

/// A "Weil du X gesehen hast" row.
#[derive(Debug, Clone, PartialEq)]
pub struct BecauseRow {
    pub source_item_id: i64,
    pub title: String,
    pub items: Vec<i64>,
}

/// Rows of titles similar to the profile's most recent strong signals.
pub async fn because_rows(
    ai: &Ai,
    viewer: &Viewer,
    rows: usize,
    per_row: usize,
) -> anyhow::Result<Vec<BecauseRow>> {
    if !ai.ensure_index().await? {
        return Ok(Vec::new());
    }
    let loaded = ai.index().await;
    let Some(loaded) = loaded.as_ref() else {
        return Ok(Vec::new());
    };
    let index = &loaded.index;
    let genres = genre_map(ai).await?;
    let taste = profile(ai, viewer.user_id, index, &genres).await?;
    let mut sources: Vec<&Signal> = taste
        .signals
        .iter()
        .filter(|s| {
            matches!(s.kind, SignalKind::Watched | SignalKind::Favorite)
                && index.vector(s.item_id).is_some()
        })
        .collect();
    sources.sort_by(|a, b| b.at.cmp(&a.at).then(a.item_id.cmp(&b.item_id)));
    let seen = taste.item_ids();
    let pool: Vec<i64> = candidates(ai, &viewer.library_ids, &viewer.restriction)
        .await?
        .into_iter()
        .filter(|id| !seen.contains(id))
        .collect();
    let names = titles(ai, sources.iter().take(rows).map(|s| s.item_id)).await?;
    let mut used: HashSet<i64> = HashSet::new();
    let mut out = Vec::new();
    for source in sources.into_iter().take(rows) {
        let Some(sv) = index.vector(source.item_id) else {
            continue;
        };
        let available: Vec<i64> = pool
            .iter()
            .copied()
            .filter(|id| !used.contains(id))
            .collect();
        let picks = rank(&available, index, |_, v| dot(sv, v) as f64, per_row, 0.85);
        if picks.len() < 3 {
            continue;
        }
        used.extend(picks.iter().map(|(id, _)| *id));
        out.push(BecauseRow {
            source_item_id: source.item_id,
            title: names.get(&source.item_id).cloned().unwrap_or_default(),
            items: picks.into_iter().map(|(id, _)| id).collect(),
        });
    }
    Ok(out)
}

/// "Mehr wie das": nearest visible neighbours of an item (episodes and seasons use
/// their series).
pub async fn similar(
    ai: &Ai,
    viewer: &Viewer,
    item_id: i64,
    limit: usize,
) -> anyhow::Result<Vec<i64>> {
    if !ai.ensure_index().await? {
        return Ok(Vec::new());
    }
    let target = match flixnet_db::items::get(ai.db(), item_id).await? {
        Some(item) if matches!(item.kind.as_str(), "episode" | "season") => {
            item.series_id.unwrap_or(item_id)
        }
        Some(_) => item_id,
        None => return Ok(Vec::new()),
    };
    let loaded = ai.index().await;
    let Some(loaded) = loaded.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(v) = loaded.index.vector(target) else {
        return Ok(Vec::new());
    };
    let allowed: HashSet<i64> = candidates(ai, &viewer.library_ids, &viewer.restriction)
        .await?
        .into_iter()
        .filter(|id| *id != target)
        .collect();
    Ok(loaded
        .index
        .search(v, limit, Some(&allowed))
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// Titles to pick from during onboarding: well rated and spread across the library.
pub async fn onboarding_candidates(
    ai: &Ai,
    viewer: &Viewer,
    limit: usize,
) -> anyhow::Result<Vec<i64>> {
    let mut filter = ItemFilter {
        library_ids: viewer.library_ids.clone(),
        kinds: vec!["movie".into(), "series".into()],
        sort: Sort::Rating,
        descending: true,
        limit: 200,
        ..Default::default()
    };
    filter.restrict(&viewer.restriction);
    let (rows, _) = browse::list(ai.db(), &filter).await?;
    let ratings: HashMap<i64, f64> = rows
        .iter()
        .map(|r| (r.id, r.community_rating.unwrap_or(5.0)))
        .collect();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    if !ai.ensure_index().await? {
        return Ok(ids.into_iter().take(limit).collect());
    }
    let loaded = ai.index().await;
    let Some(loaded) = loaded.as_ref() else {
        return Ok(ids.into_iter().take(limit).collect());
    };
    let mut picks: Vec<i64> = rank(
        &ids,
        &loaded.index,
        |id, _| ratings.get(&id).copied().unwrap_or(5.0) / 10.0,
        limit,
        0.5,
    )
    .into_iter()
    .map(|(id, _)| id)
    .collect();
    // Items without vectors still fill up the list.
    for id in ids {
        if picks.len() >= limit {
            break;
        }
        if !picks.contains(&id) {
            picks.push(id);
        }
    }
    Ok(picks)
}

/// One suggestion of the group mode.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSuggestion {
    pub item_id: i64,
    pub score: f64,
    pub reason: String,
    /// Members this title suits especially well.
    pub matches: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupList {
    pub items: Vec<GroupSuggestion>,
    /// Members without any history or onboarding picks (they do not shape the list).
    pub without_history: Vec<i64>,
    pub available: bool,
}

/// "Was schauen wir heute?": suggestions for several profiles at once. `library_ids`
/// and `restriction` must already be the intersection / strictest combination of all
/// members. Scores combine the members' tastes (average plus least misery).
pub async fn for_group(
    ai: &Ai,
    members: &[Viewer],
    library_ids: &[i64],
    restriction: &Restriction,
    exclude_watched: bool,
    limit: usize,
) -> anyhow::Result<GroupList> {
    if !ai.ensure_index().await? {
        return Ok(GroupList::default());
    }
    let loaded = ai.index().await;
    let Some(loaded) = loaded.as_ref() else {
        return Ok(GroupList::default());
    };
    let index = &loaded.index;
    let genres = genre_map(ai).await?;
    let mut profiles: Vec<(&Viewer, TasteProfile)> = Vec::new();
    let mut without_history = Vec::new();
    for member in members {
        let taste = profile(ai, member.user_id, index, &genres).await?;
        if taste.is_empty() {
            without_history.push(member.user_id);
        } else {
            profiles.push((member, taste));
        }
    }
    let member_ids: HashSet<i64> = members.iter().map(|m| m.user_id).collect();
    let mut excluded: HashSet<i64> = HashSet::new();
    if exclude_watched {
        for (user, item) in flixnet_db::ai::household_watched(ai.db()).await? {
            if member_ids.contains(&user) {
                excluded.insert(item);
            }
        }
        for (_, taste) in &profiles {
            excluded.extend(
                taste
                    .signals
                    .iter()
                    .filter(|s| matches!(s.kind, SignalKind::Watched | SignalKind::Started))
                    .map(|s| s.item_id),
            );
        }
    }
    let pool: Vec<i64> = candidates(ai, library_ids, restriction)
        .await?
        .into_iter()
        .filter(|id| !excluded.contains(id))
        .collect();
    if profiles.is_empty() {
        return Ok(GroupList {
            items: Vec::new(),
            without_history,
            available: true,
        });
    }

    // Each member's scores, min-max normalized over the pool so tastes weigh equally.
    let normalized: Vec<HashMap<i64, f64>> = profiles
        .iter()
        .map(|(_, taste)| {
            let raw: Vec<(i64, f64)> = pool
                .iter()
                .filter_map(|id| {
                    index
                        .vector(*id)
                        .map(|v| (*id, taste.item_score(*id, v, &genres)))
                })
                .collect();
            let (lo, hi) = raw.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (_, s)| {
                (lo.min(*s), hi.max(*s))
            });
            let span = (hi - lo).max(1e-9);
            raw.into_iter()
                .map(|(id, s)| (id, (s - lo) / span))
                .collect()
        })
        .collect();
    let combined = |id: i64| -> f64 {
        let scores: Vec<f64> = normalized
            .iter()
            .map(|m| m.get(&id).copied().unwrap_or(0.0))
            .collect();
        let mean = scores.iter().sum::<f64>() / scores.len() as f64;
        let min = scores.iter().copied().fold(f64::MAX, f64::min);
        0.5 * mean + 0.5 * min
    };
    let picks = rank(&pool, index, |id, _| combined(id), limit, MMR_LAMBDA);
    let items = picks
        .into_iter()
        .map(|(item_id, score)| {
            let matches: Vec<&Viewer> = profiles
                .iter()
                .zip(&normalized)
                .filter(|(_, n)| n.get(&item_id).copied().unwrap_or(0.0) >= 0.75)
                .map(|((m, _), _)| *m)
                .collect();
            let reason = if profiles.len() > 1 && matches.len() == profiles.len() {
                "Passt zu allen".to_owned()
            } else if matches.is_empty() {
                "Ein guter Kompromiss".to_owned()
            } else {
                format!(
                    "Passt besonders zu {}",
                    join_names(&matches.iter().map(|m| m.name.as_str()).collect::<Vec<_>>())
                )
            };
            GroupSuggestion {
                item_id,
                score,
                reason,
                matches: matches.iter().map(|m| m.user_id).collect(),
            }
        })
        .collect();
    Ok(GroupList {
        items,
        without_history,
        available: true,
    })
}

/// "Anna", "Anna und Ben", "Anna, Ben und Cem".
pub fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} und {last}", rest.join(", ")),
    }
}

/// Builds an in-memory index (tests and offline evaluation).
pub fn index_from(vectors: &[(i64, Vec<f32>)]) -> BruteForceIndex {
    let dims = vectors.first().map_or(0, |(_, v)| v.len());
    let mut index = BruteForceIndex::new(dims);
    for (id, v) in vectors {
        let mut v = v.clone();
        math::normalize(&mut v);
        index.insert(*id, &v);
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 3600 * 1000;

    fn row(item_id: i64, played: bool, at: i64) -> ViewingRow {
        ViewingRow {
            item_id,
            played,
            play_count: i64::from(played),
            favorite: false,
            rating: None,
            position_ms: 0,
            watched_fraction: if played { 1.0 } else { 0.0 },
            at,
        }
    }

    #[test]
    fn signals_weigh_watching_liking_and_age() {
        let now = 1_000 * DAY;
        let mut fav = row(2, true, now);
        fav.favorite = true;
        let mut started = row(3, false, now);
        started.position_ms = 600_000;
        started.watched_fraction = 0.5;
        let mut disliked = row(4, true, now);
        disliked.rating = Some(2.0);
        let rows = vec![
            row(1, true, now - 180 * DAY), // half a year ago: half the weight
            fav,
            started,
            disliked,
            // A series: three episodes aggregate, sub-linearly.
            row(5, true, now),
            row(5, true, now),
            row(5, true, now),
        ];
        let signals = signals_from(&rows, &[(6, now), (2, now)], now);
        let get = |id| signals.iter().find(|s| s.item_id == id);
        assert!((get(1).unwrap().weight - 0.5).abs() < 1e-9);
        assert_eq!(get(1).unwrap().kind, SignalKind::Watched);
        assert_eq!(
            get(2).unwrap().kind,
            SignalKind::Favorite,
            "a pick of something liked stays a favorite"
        );
        assert!((get(2).unwrap().weight - (1.0 + 2.0_f64.ln())).abs() < 1e-9);
        assert_eq!(get(3).unwrap().kind, SignalKind::Started);
        assert!((get(3).unwrap().weight - 0.55).abs() < 1e-9);
        assert!(get(4).is_none(), "a bad rating cancels the signal");
        assert!((get(5).unwrap().weight - (1.0 + 3.0_f64.ln())).abs() < 1e-9);
        assert_eq!(get(6).unwrap().kind, SignalKind::Seed);
        // Strongest first.
        assert!(signals.windows(2).all(|w| w[0].weight >= w[1].weight));
    }

    #[test]
    fn cluster_count_grows_slowly() {
        assert_eq!(cluster_count(1), 1);
        assert_eq!(cluster_count(3), 1);
        assert_eq!(cluster_count(8), 2);
        assert_eq!(cluster_count(18), 3);
        assert_eq!(cluster_count(500), 5);
    }

    #[test]
    fn two_interests_give_two_clusters_and_both_get_recommended() {
        // Dimension 0: space films, dimension 1: romance, dimension 2: horror.
        let index = index_from(&[
            (1, vec![1.0, 0.1, 0.0]),
            (2, vec![0.95, 0.0, 0.1]),
            (3, vec![1.0, 0.0, 0.05]),
            (4, vec![0.9, 0.05, 0.0]),
            (5, vec![0.0, 1.0, 0.05]),
            (6, vec![0.1, 0.95, 0.0]),
            (10, vec![0.97, 0.03, 0.0]),  // unseen space film
            (11, vec![0.96, 0.04, 0.02]), // its near-duplicate
            (12, vec![0.05, 1.0, 0.0]),   // unseen romance
            (13, vec![0.0, 0.0, 1.0]),    // horror: not their taste
        ]);
        let now = 100 * DAY;
        let rows: Vec<ViewingRow> = [1, 2, 3, 4, 5, 6]
            .iter()
            .map(|id| row(*id, true, now))
            .collect();
        let taste = TasteProfile::build(signals_from(&rows, &[], now), &index);
        assert_eq!(taste.clusters.len(), 2);
        let picks = rank(
            &[10, 11, 12, 13],
            &index,
            |_, v| taste.score(v),
            2,
            MMR_LAMBDA,
        );
        let ids: Vec<i64> = picks.iter().map(|p| p.0).collect();
        assert!(matches!(ids[0], 10 | 11), "{ids:?}");
        assert_eq!(ids[1], 12, "MMR takes the romance over the near-duplicate");
        let closest = taste
            .closest_signal(index.vector(12).unwrap(), &index)
            .unwrap();
        assert!(matches!(closest.item_id, 5 | 6));
        assert_eq!(
            reason_for(closest, "Notting Hill"),
            "Weil du „Notting Hill“ gesehen hast"
        );
    }

    #[test]
    fn cowatch_follows_profiles_with_overlapping_history() {
        let watched = [
            (1, 10),
            (1, 11),
            (2, 10),
            (2, 11),
            (2, 12),
            (3, 13),
            (3, 14),
        ];
        let scores = cowatch_scores(1, &watched);
        assert!((scores[&12] - 1.0).abs() < 1e-9, "only profile 2 overlaps");
        assert!(!scores.contains_key(&13), "profile 3 shares nothing");
        assert!(!scores.contains_key(&10), "own titles are not candidates");
        assert!(cowatch_scores(9, &watched).is_empty());
    }

    #[test]
    fn strictest_restriction_hides_what_any_member_must_not_see() {
        let kid = Restriction {
            max_age: Some(6),
            allow_unrated: false,
            blocked_genres: vec!["Horror".into()],
        };
        let teen = Restriction {
            max_age: Some(12),
            allow_unrated: true,
            blocked_genres: vec!["horror".into(), "Krieg".into()],
        };
        let adult = Restriction::default();
        let combined = strictest(&[adult.clone(), teen.clone(), kid.clone()]);
        assert_eq!(combined.max_age, Some(6));
        assert!(!combined.allow_unrated);
        assert_eq!(combined.blocked_genres, ["horror", "Krieg"]);
        for r in [&kid, &teen, &adult] {
            assert!(combined.at_least_as_strict_as(r));
        }
        let open = strictest(&[
            Restriction {
                max_age: Some(16),
                allow_unrated: true,
                blocked_genres: vec![],
            },
            adult.clone(),
        ]);
        assert!(open.allow_unrated && open.max_age == Some(16));
        assert_eq!(strictest(&[adult.clone(), adult]), Restriction::default());
        assert_eq!(join_names(&["Anna", "Ben", "Cem"]), "Anna, Ben und Cem");
        assert_eq!(join_names(&["Anna"]), "Anna");
    }
}
