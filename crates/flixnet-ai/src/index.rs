//! Vector search behind a small trait. The implementation is a brute-force scan over
//! an in-memory copy of `item_embeddings` (contiguous f32, dot products on unit
//! vectors): a few ms for 20k items × 768 dims, fine up to ~100k items. sqlite-vec was
//! the alternative; it would need a C extension registered via unsafe FFI next to
//! sqlx's bundled SQLite on every platform, for no gain at library sizes.

use std::collections::{HashMap, HashSet};

use crate::math::dot;

pub trait VectorIndex: Send + Sync {
    fn dimensions(&self) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The stored vector of an item.
    fn vector(&self, id: i64) -> Option<&[f32]>;
    /// The `k` most similar items (cosine, best first), optionally only among `allow`.
    fn search(&self, query: &[f32], k: usize, allow: Option<&HashSet<i64>>) -> Vec<(i64, f32)>;
}

#[derive(Debug, Clone, Default)]
pub struct BruteForceIndex {
    dimensions: usize,
    ids: Vec<i64>,
    data: Vec<f32>,
    position: HashMap<i64, usize>,
}

impl BruteForceIndex {
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            ..Default::default()
        }
    }

    /// Adds (or replaces) a vector; vectors of the wrong size are ignored.
    pub fn insert(&mut self, id: i64, vector: &[f32]) {
        if vector.len() != self.dimensions {
            return;
        }
        if let Some(&pos) = self.position.get(&id) {
            self.data[pos * self.dimensions..(pos + 1) * self.dimensions].copy_from_slice(vector);
        } else {
            self.position.insert(id, self.ids.len());
            self.ids.push(id);
            self.data.extend_from_slice(vector);
        }
    }

    pub fn ids(&self) -> &[i64] {
        &self.ids
    }
}

impl VectorIndex for BruteForceIndex {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn vector(&self, id: i64) -> Option<&[f32]> {
        let pos = *self.position.get(&id)?;
        Some(&self.data[pos * self.dimensions..(pos + 1) * self.dimensions])
    }

    fn search(&self, query: &[f32], k: usize, allow: Option<&HashSet<i64>>) -> Vec<(i64, f32)> {
        if query.len() != self.dimensions || k == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(i64, f32)> = self
            .ids
            .iter()
            .enumerate()
            .filter(|(_, id)| allow.is_none_or(|a| a.contains(id)))
            .map(|(pos, id)| {
                let v = &self.data[pos * self.dimensions..(pos + 1) * self.dimensions];
                (*id, dot(query, v))
            })
            .collect();
        let by_score = |a: &(i64, f32), b: &(i64, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
        if scored.len() > k {
            scored.select_nth_unstable_by(k - 1, by_score);
            scored.truncate(k);
        }
        scored.sort_by(by_score);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nearest_neighbours_within_the_allowed_set() {
        let mut index = BruteForceIndex::new(2);
        index.insert(1, &[1.0, 0.0]);
        index.insert(2, &[0.8, 0.6]);
        index.insert(3, &[0.0, 1.0]);
        index.insert(4, &[1.0, 0.0, 0.0]); // wrong size: ignored
        assert_eq!(index.len(), 3);
        let hits = index.search(&[1.0, 0.0], 2, None);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [1, 2]);
        assert!((hits[1].1 - 0.8).abs() < 1e-6);
        let allow: HashSet<i64> = [2, 3].into();
        let hits = index.search(&[1.0, 0.0], 5, Some(&allow));
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [2, 3]);
        index.insert(3, &[1.0, 0.0]);
        assert_eq!(index.vector(3), Some(&[1.0, 0.0][..]));
        assert!(index.search(&[1.0], 3, None).is_empty());
    }
}
