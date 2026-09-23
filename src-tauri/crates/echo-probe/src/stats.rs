//! Robust summaries: medians rather than means everywhere, because a lag track
//! always carries a few windows where the correlation locked onto the wrong
//! peak, and one of those should not move an answer.

pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v: Vec<f64> = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 })
}

/// The `q` quantile (0..=1), nearest rank.
pub fn percentile(values: &[f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v: Vec<f64> = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let rank = ((q.clamp(0.0, 1.0) * (v.len() - 1) as f64).round()) as usize;
    Some(v[rank])
}

/// Median absolute deviation from the median.
pub fn mad(values: &[f64]) -> Option<f64> {
    let m = median(values)?;
    let dev: Vec<f64> = values.iter().map(|v| (v - m).abs()).collect();
    median(&dev)
}

/// Theil–Sen line through `(x, y)` points: the median of every pairwise slope,
/// then the median intercept under it. `None` below two distinct x values.
pub fn theil_sen(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let mut slopes = Vec::new();
    for i in 0..points.len() {
        for j in i + 1..points.len() {
            let dx = points[j].0 - points[i].0;
            if dx.abs() > 1e-9 {
                slopes.push((points[j].1 - points[i].1) / dx);
            }
        }
    }
    let slope = median(&slopes)?;
    let intercepts: Vec<f64> = points.iter().map(|(x, y)| y - slope * x).collect();
    Some((slope, median(&intercepts)?))
}

pub fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / x.len() as f64).sqrt()
}

pub fn dbfs(rms: f64) -> f64 {
    20.0 * rms.max(1e-10).log10()
}

pub fn db_ratio(num: f64, den: f64) -> Option<f64> {
    if num > 0.0 && den > 0.0 {
        Some(10.0 * (num / den).log10())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theil_sen_ignores_an_outlier() {
        let mut pts: Vec<(f64, f64)> = (0..20).map(|i| (i as f64, 2.0 * i as f64 + 1.0)).collect();
        pts[7].1 = 500.0;
        let (slope, intercept) = theil_sen(&pts).unwrap();
        assert!((slope - 2.0).abs() < 1e-9 && (intercept - 1.0).abs() < 1e-9);
    }

    #[test]
    fn median_of_even_and_odd() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[]), None);
    }
}
