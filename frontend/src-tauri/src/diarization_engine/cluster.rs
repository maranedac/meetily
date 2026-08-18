//! Simple agglomerative clustering over speaker embeddings.
//!
//! Deliberately not a crate dependency: for a single meeting's "system" track,
//! segment counts are small (tens, maybe low hundreds), so a plain O(n^2) pairwise
//! distance + greedy merge is more than fast enough, and keeps this dependency-free.

/// Cosine distance (0 = identical direction, 2 = opposite). Using distance (not
/// similarity) so "merge while below threshold" reads naturally.
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0; // treat degenerate (all-zero) embeddings as maximally dissimilar
    }
    1.0 - (dot / (norm_a * norm_b))
}

/// Distance threshold below which two clusters are considered the same speaker.
/// Cosine-distance thresholds for WeSpeaker-family embeddings typically fall in the
/// 0.15-0.3 range in practice; this starting value has NOT been empirically tuned
/// against real meeting audio yet - revisit if speakers are consistently over- or
/// under-split.
const MERGE_DISTANCE_THRESHOLD: f32 = 0.25;

/// Average-linkage agglomerative clustering: repeatedly merge the two closest
/// clusters (by mean pairwise distance between their members) until the closest
/// remaining pair exceeds the threshold. Returns a cluster id (0-based) per input
/// embedding, in the same order as the input.
pub fn cluster_embeddings(embeddings: &[Vec<f32>]) -> Vec<usize> {
    let n = embeddings.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0];
    }

    // Precompute the full pairwise distance matrix once.
    let mut dist = vec![vec![0.0f32; n]; n];
    for i in 0..n {
        for j in (i + 1)..n {
            let d = cosine_distance(&embeddings[i], &embeddings[j]);
            dist[i][j] = d;
            dist[j][i] = d;
        }
    }

    // Each cluster starts as its own singleton, tracked by member indices.
    let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();

    loop {
        if clusters.len() <= 1 {
            break;
        }

        // Find the closest pair of clusters by average-linkage distance.
        let mut best: Option<(usize, usize, f32)> = None;
        for a in 0..clusters.len() {
            for b in (a + 1)..clusters.len() {
                let mut sum = 0.0f32;
                let mut count = 0usize;
                for &i in &clusters[a] {
                    for &j in &clusters[b] {
                        sum += dist[i][j];
                        count += 1;
                    }
                }
                let avg = sum / count.max(1) as f32;
                if best.map_or(true, |(_, _, best_d)| avg < best_d) {
                    best = Some((a, b, avg));
                }
            }
        }

        match best {
            Some((a, b, d)) if d <= MERGE_DISTANCE_THRESHOLD => {
                // Merge b into a, remove b (swap_remove-safe since b > a and we
                // only ever remove the higher index within this iteration).
                let merged = clusters[b].clone();
                clusters[a].extend(merged);
                clusters.remove(b);
            }
            _ => break, // closest pair is already too far apart - done merging
        }
    }

    // Map back to a per-embedding cluster id.
    let mut labels = vec![0usize; n];
    for (cluster_id, members) in clusters.iter().enumerate() {
        for &member in members {
            labels[member] = cluster_id;
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec3(x: f32, y: f32, z: f32) -> Vec<f32> {
        vec![x, y, z]
    }

    #[test]
    fn single_embedding_is_one_cluster() {
        assert_eq!(cluster_embeddings(&[vec3(1.0, 0.0, 0.0)]), vec![0]);
    }

    #[test]
    fn near_identical_embeddings_merge() {
        let embeddings = vec![
            vec3(1.0, 0.0, 0.0),
            vec3(0.98, 0.02, 0.0),
            vec3(0.0, 1.0, 0.0),
        ];
        let labels = cluster_embeddings(&embeddings);
        assert_eq!(labels[0], labels[1], "near-identical embeddings should cluster together");
        assert_ne!(labels[0], labels[2], "orthogonal embedding should be a different cluster");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(cluster_embeddings(&[]).is_empty());
    }
}
