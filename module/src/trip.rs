//! A good order to visit some points: the nearest one each time, improved by reversing stretches
//! (2-opt) while that shortens the trip. Not always the best order, but close to it and fast.

/// The order of the points, from the first, by the matrix of their durations; `round`, back to it at
/// the end. `None` if some point cannot be reached.
pub fn order(durations: &[Vec<Option<f64>>], round: bool) -> Option<Vec<usize>> {
    let n = durations.len();
    if n == 0 {
        return Some(Vec::new());
    }
    let cost = |a: usize, b: usize| durations[a][b];
    let mut visited = vec![false; n];
    visited[0] = true;
    let mut path = vec![0];
    while path.len() < n {
        let at = *path.last()?;
        let next = (0..n).filter(|p| !visited[*p]).filter_map(|p| Some((cost(at, p)?, p))).min_by(|a, b| a.0.total_cmp(&b.0))?.1;
        visited[next] = true;
        path.push(next);
    }
    let total = |path: &[usize]| -> Option<f64> {
        let legs: Option<f64> = path.windows(2).map(|w| cost(w[0], w[1])).sum();
        Some(legs? + if round { cost(*path.last()?, path[0])? } else { 0.0 })
    };
    let mut best = total(&path)?;
    let mut improved = true;
    while improved {
        improved = false;
        for i in 1..n.saturating_sub(1) {
            for j in i + 1..n {
                let mut other = path.clone();
                other[i..=j].reverse();
                if let Some(length) = total(&other).filter(|l| *l + 1e-9 < best) {
                    (path, best) = (other, length);
                    improved = true;
                }
            }
        }
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points on a line, in this order, a second per unit.
    fn on_a_line(at: &[f64]) -> Vec<Vec<Option<f64>>> {
        at.iter().map(|a| at.iter().map(|b| Some((a - b).abs())).collect()).collect()
    }

    #[test]
    fn it_goes_along_the_line_and_starts_at_the_first() {
        let order = order(&on_a_line(&[0.0, 30.0, 10.0, 20.0]), false).unwrap();
        assert_eq!(order, [0, 2, 3, 1]);
    }

    #[test]
    fn it_undoes_a_crossing_that_the_nearest_one_leaves() {
        // The corners of a square, the nearest first leading to a long way back.
        let points = [(0.0, 0.0), (1.0, 0.1), (2.0, 0.0), (2.0, 2.0), (1.0, 2.1), (0.0, 2.0), (1.0, 1.0)];
        let matrix: Vec<Vec<Option<f64>>> = points.iter().map(|a: &(f64, f64)| points.iter().map(|b| Some(((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt())).collect()).collect();
        let found = order(&matrix, true).unwrap();
        let length: f64 = (0..found.len()).map(|i| matrix[found[i]][found[(i + 1) % found.len()]].unwrap()).sum();
        assert_eq!(found[0], 0);
        assert!(length < 8.9, "{length}: {found:?}");
    }

    #[test]
    fn a_point_out_of_reach_has_no_order() {
        let mut matrix = on_a_line(&[0.0, 1.0, 2.0]);
        matrix.iter_mut().for_each(|row| row[2] = None);
        assert_eq!(order(&matrix, false), None);
        assert_eq!(order(&[], true), Some(vec![]));
    }
}
