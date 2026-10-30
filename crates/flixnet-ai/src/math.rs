//! Vector and ranking math: cosine, Reciprocal Rank Fusion, time decay, spherical
//! k-means and Maximal Marginal Relevance. Pure functions, unit-tested below.

use std::collections::HashMap;

/// Dot product (= cosine similarity for unit vectors).
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Cosine similarity; 0 when either vector is zero.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (na, nb) = (norm(a), norm(b));
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot(a, b) / (na * nb)
    }
}

pub fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Scales to unit length (zero vectors stay zero).
pub fn normalize(v: &mut [f32]) {
    let n = norm(v);
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// Little-endian f32 bytes (the storage format of `item_embeddings.vector`).
pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// The usual RRF constant: dampens the influence of the very top ranks.
pub const RRF_K: f64 = 60.0;

/// Reciprocal Rank Fusion: `score(d) = Σ 1 / (k + rank)` over every ranked list the
/// document appears in (ranks start at 1). Highest score first; ties keep the order
/// of first appearance.
pub fn rrf(lists: &[&[i64]], k: f64) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, (f64, usize)> = HashMap::new();
    let mut seen = 0;
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            let entry = scores.entry(*id).or_insert_with(|| {
                seen += 1;
                (0.0, seen)
            });
            entry.0 += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<(i64, f64, usize)> =
        scores.into_iter().map(|(id, (s, o))| (id, s, o)).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    out.into_iter().map(|(id, s, _)| (id, s)).collect()
}

/// Half-life of a viewing signal: something watched 180 days ago counts half.
pub const HALF_LIFE_MS: f64 = 180.0 * 24.0 * 3600.0 * 1000.0;

/// Exponential time decay: 1 now, 0.5 after one half-life. Future timestamps count 1.
pub fn decay(age_ms: i64, half_life_ms: f64) -> f64 {
    if age_ms <= 0 {
        1.0
    } else {
        0.5_f64.powf(age_ms as f64 / half_life_ms)
    }
}

/// Weighted mean of unit vectors, normalized again (a taste centroid).
pub fn centroid(vectors: &[&[f32]], weights: &[f64]) -> Option<Vec<f32>> {
    let dims = vectors.first()?.len();
    let mut sum = vec![0.0_f32; dims];
    let mut total = 0.0;
    for (v, w) in vectors.iter().zip(weights) {
        if *w <= 0.0 {
            continue;
        }
        total += w;
        for (s, x) in sum.iter_mut().zip(v.iter()) {
            *s += x * *w as f32;
        }
    }
    if total <= 0.0 {
        return None;
    }
    normalize(&mut sum);
    Some(sum)
}

/// Small deterministic PRNG (xorshift64*), so clustering is reproducible.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One cluster of a k-means result.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub centroid: Vec<f32>,
    /// Indices into the input.
    pub members: Vec<usize>,
    /// Sum of the members' weights.
    pub weight: f64,
}

/// Weighted spherical k-means (cosine) with k-means++ seeding. Deterministic for a
/// given input. Empty clusters are dropped, so fewer than `k` may come back.
pub fn kmeans(vectors: &[&[f32]], weights: &[f64], k: usize, iterations: usize) -> Vec<Cluster> {
    let n = vectors.len();
    if n == 0 || k == 0 {
        return Vec::new();
    }
    let k = k.min(n);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ n as u64);

    // k-means++: first the heaviest point, then proportional to weight × distance².
    let first = (0..n)
        .max_by(|a, b| weights[*a].total_cmp(&weights[*b]).then(b.cmp(a)))
        .unwrap_or(0);
    let mut centroids: Vec<Vec<f32>> = vec![vectors[first].to_vec()];
    while centroids.len() < k {
        let dist: Vec<f64> = (0..n)
            .map(|i| {
                let best = centroids
                    .iter()
                    .map(|c| dot(vectors[i], c))
                    .fold(f32::MIN, f32::max);
                let d = (1.0 - best as f64).max(0.0);
                d * d * weights[i].max(1e-9)
            })
            .collect();
        let total: f64 = dist.iter().sum();
        if total <= 1e-12 {
            break; // all points coincide with a centroid
        }
        let mut target = rng.next_f64() * total;
        let mut pick = n - 1;
        for (i, d) in dist.iter().enumerate() {
            if target < *d {
                pick = i;
                break;
            }
            target -= d;
        }
        centroids.push(vectors[pick].to_vec());
    }

    let mut assignment = vec![usize::MAX; n];
    for _ in 0..iterations.max(1) {
        let mut changed = false;
        for (i, v) in vectors.iter().enumerate() {
            let best = (0..centroids.len())
                .max_by(|a, b| {
                    dot(v, &centroids[*a])
                        .total_cmp(&dot(v, &centroids[*b]))
                        .then(b.cmp(a))
                })
                .unwrap_or(0);
            if assignment[i] != best {
                assignment[i] = best;
                changed = true;
            }
        }
        for (c, centroid_vec) in centroids.iter_mut().enumerate() {
            let (members, member_weights): (Vec<&[f32]>, Vec<f64>) = (0..n)
                .filter(|i| assignment[*i] == c)
                .map(|i| (vectors[i], weights[i]))
                .unzip();
            if let Some(next) = centroid(&members, &member_weights) {
                *centroid_vec = next;
            }
        }
        if !changed {
            break;
        }
    }

    centroids
        .into_iter()
        .enumerate()
        .filter_map(|(c, centroid)| {
            let members: Vec<usize> = (0..n).filter(|i| assignment[*i] == c).collect();
            (!members.is_empty()).then(|| Cluster {
                weight: members.iter().map(|i| weights[*i]).sum(),
                centroid,
                members,
            })
        })
        .collect()
}

/// The λ used for recommendations: mostly relevance, some diversity.
pub const MMR_LAMBDA: f64 = 0.7;

/// Maximal Marginal Relevance: picks `count` candidates one by one, each maximizing
/// `λ·relevance − (1−λ)·max similarity to what is already picked`. `relevance` should
/// be on a 0–1 scale; `similarity(i, j)` compares two candidates by index.
pub fn mmr(
    relevance: &[f64],
    count: usize,
    lambda: f64,
    similarity: impl Fn(usize, usize) -> f64,
) -> Vec<usize> {
    let mut picked: Vec<usize> = Vec::new();
    let mut max_sim = vec![f64::MIN; relevance.len()];
    let mut available: Vec<bool> = vec![true; relevance.len()];
    while picked.len() < count.min(relevance.len()) {
        let mut best: Option<(usize, f64)> = None;
        for i in 0..relevance.len() {
            if !available[i] {
                continue;
            }
            let penalty = if picked.is_empty() { 0.0 } else { max_sim[i] };
            let score = lambda * relevance[i] - (1.0 - lambda) * penalty;
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((i, score));
            }
        }
        let Some((chosen, _)) = best else { break };
        available[chosen] = false;
        picked.push(chosen);
        for i in 0..relevance.len() {
            if available[i] {
                max_sim[i] = max_sim[i].max(similarity(i, chosen));
            }
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let mut v = v.to_vec();
        normalize(&mut v);
        v
    }

    #[test]
    fn cosine_of_parallel_orthogonal_and_opposite_vectors() {
        assert!((cosine(&[1.0, 2.0], &[2.0, 4.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 3.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 1.0], &[-1.0, -1.0]) + 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        let v = unit(&[3.0, 4.0]);
        assert!((norm(&v) - 1.0).abs() < 1e-6);
        assert_eq!(from_bytes(&to_bytes(&v)), v);
    }

    #[test]
    fn rrf_rewards_documents_ranked_well_in_several_lists() {
        // b is second in both lists; a is first in one list only.
        let fused = rrf(&[&[1, 2, 3], &[4, 2, 1]], RRF_K);
        let order: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
        assert_eq!(order[0], 1, "1 is ranked 1st and 3rd: 1/61 + 1/63");
        assert_eq!(order[1], 2, "2 is 2nd twice: 2/62");
        assert!((fused[0].1 - (1.0 / 61.0 + 1.0 / 63.0)).abs() < 1e-12);
        assert!((fused[1].1 - 2.0 / 62.0).abs() < 1e-12);
        // Documents in one list only: by rank, earlier list first on ties.
        assert_eq!(&order[2..], &[4, 3]);
        assert!(rrf(&[], RRF_K).is_empty());
    }

    #[test]
    fn decay_halves_every_half_life() {
        assert_eq!(decay(0, HALF_LIFE_MS), 1.0);
        assert_eq!(decay(-5, HALF_LIFE_MS), 1.0);
        assert!((decay(HALF_LIFE_MS as i64, HALF_LIFE_MS) - 0.5).abs() < 1e-9);
        assert!((decay(2 * HALF_LIFE_MS as i64, HALF_LIFE_MS) - 0.25).abs() < 1e-9);
        assert!(decay(30 * 24 * 3600 * 1000, HALF_LIFE_MS) > 0.89);
    }

    #[test]
    fn kmeans_separates_two_obvious_groups() {
        let points: Vec<Vec<f32>> = [
            [1.0, 0.1, 0.0],
            [0.9, 0.0, 0.1],
            [1.0, 0.05, 0.05],
            [0.0, 1.0, 0.1],
            [0.1, 0.9, 0.0],
        ]
        .iter()
        .map(|p| unit(p))
        .collect();
        let refs: Vec<&[f32]> = points.iter().map(Vec::as_slice).collect();
        let clusters = kmeans(&refs, &[1.0; 5], 2, 20);
        assert_eq!(clusters.len(), 2);
        let mut groups: Vec<Vec<usize>> = clusters.iter().map(|c| c.members.clone()).collect();
        groups.sort();
        assert_eq!(groups, vec![vec![0, 1, 2], vec![3, 4]]);
        let big = clusters.iter().find(|c| c.members.len() == 3).unwrap();
        assert!((big.weight - 3.0).abs() < 1e-9);
        assert!(big.centroid[0] > 0.9);
        // Deterministic, and k larger than n is fine.
        assert_eq!(kmeans(&refs, &[1.0; 5], 2, 20), clusters);
        assert_eq!(kmeans(&refs[..1], &[1.0], 4, 10).len(), 1);
        assert!(kmeans(&[], &[], 3, 10).is_empty());
    }

    #[test]
    fn weighted_centroid_leans_towards_heavier_points() {
        let a = [1.0_f32, 0.0];
        let b = [0.0_f32, 1.0];
        let c = centroid(&[&a, &b], &[3.0, 1.0]).unwrap();
        assert!(c[0] > c[1]);
        assert!((norm(&c) - 1.0).abs() < 1e-6);
        assert!(centroid(&[&a], &[0.0]).is_none());
    }

    #[test]
    fn mmr_trades_relevance_for_diversity() {
        // 0 and 1 are near-duplicates; 2 is different but a bit less relevant.
        let vectors = [unit(&[1.0, 0.0]), unit(&[0.99, 0.01]), unit(&[0.0, 1.0])];
        let sim = |i: usize, j: usize| dot(&vectors[i], &vectors[j]) as f64;
        let relevance = [1.0, 0.95, 0.7];
        assert_eq!(mmr(&relevance, 2, MMR_LAMBDA, sim), vec![0, 2]);
        // λ = 1 is pure relevance.
        assert_eq!(mmr(&relevance, 3, 1.0, sim), vec![0, 1, 2]);
        assert_eq!(mmr(&relevance, 10, MMR_LAMBDA, sim).len(), 3);
        assert!(mmr(&[], 3, MMR_LAMBDA, sim).is_empty());
    }
}
