pub fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty() || left.len() != right.len() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;
    for (l, r) in left.iter().zip(right.iter()) {
        let l = f64::from(*l);
        let r = f64::from(*r);
        dot += l * r;
        left_norm += l * l;
        right_norm += r * r;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return None;
    }
    Some(dot / (left_norm.sqrt() * right_norm.sqrt()))
}

pub fn reciprocal_rank_fusion(ranked_lists: &[Vec<(String, f64)>], k: f64) -> Vec<(String, f64)> {
    let mut scores = std::collections::HashMap::<String, f64>::new();
    for list in ranked_lists {
        for (rank, (key, _score)) in list.iter().enumerate() {
            let fused = 1.0 / (k + rank as f64 + 1.0);
            *scores.entry(key.clone()).or_insert(0.0) += fused;
        }
    }
    let mut items: Vec<_> = scores.into_iter().collect();
    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    items
}
