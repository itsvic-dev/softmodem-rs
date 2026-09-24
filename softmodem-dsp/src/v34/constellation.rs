//! The 1664-point superconstellation of § 9.1 and figure 5.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Points in the quarter-superconstellation of figure 5.
pub const QUARTER: usize = 416;

/// A point on the grid of odd integers.
pub type Point = (i32, i32);

static QUARTER_POINTS: LazyLock<Vec<Point>> = LazyLock::new(|| {
    let coordinates = || (-47..=49).step_by(4);
    let mut points: Vec<Point> = coordinates()
        .flat_map(|x| coordinates().map(move |y| (x, y)))
        .collect();
    points.sort_by_key(|&(x, y)| (x * x + y * y, -y));
    points.truncate(QUARTER);
    points
});

static LABELS: LazyLock<HashMap<Point, usize>> = LazyLock::new(|| {
    QUARTER_POINTS
        .iter()
        .enumerate()
        .map(|(label, &point)| (point, label))
        .collect()
});

/// The point that figure 5 labels `label`, below `QUARTER`.
#[must_use]
pub fn point(label: usize) -> Point {
    QUARTER_POINTS[label]
}

/// Turned clockwise by `quarter_turns` · 90°, as § 9.6.1 turns points.
#[must_use]
pub fn rotate(point: Point, quarter_turns: u8) -> Point {
    (0..quarter_turns % 4).fold(point, |(x, y), _| (y, -x))
}

/// The label and clockwise quarter turns that give `point`, if it is in the
/// superconstellation.
#[must_use]
pub fn label(point: Point) -> Option<(usize, u8)> {
    (0..4).find_map(|turns| {
        let quarter = rotate(point, (4 - turns) % 4);
        LABELS.get(&quarter).map(|&label| (label, turns))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(y: i32) -> Vec<usize> {
        let mut found: Vec<(i32, usize)> = (0..QUARTER)
            .filter(|&label| point(label).1 == y)
            .map(|label| (point(label).0, label))
            .collect();
        found.sort_unstable();
        found.into_iter().map(|(_, label)| label).collect()
    }

    #[test]
    fn labels_the_rows_of_figure_5() {
        assert_eq!(
            row(1),
            [
                362, 296, 238, 186, 142, 103, 69, 43, 22, 9, 1, 0, 5, 16, 32, 56, 85, 122, 163,
                213, 267, 328, 395
            ]
        );
        assert_eq!(
            row(-3),
            [
                365, 300, 240, 190, 144, 106, 73, 45, 25, 11, 3, 2, 7, 18, 36, 59, 88, 124, 166,
                217, 272, 331, 397
            ]
        );
        assert_eq!(row(45), [408, 396, 394, 400, 414]);
        assert_eq!(row(-43), [411, 389, 374, 366, 364, 368, 381, 393]);
        assert_eq!(
            row(21),
            [
                384, 324, 277, 229, 189, 156, 131, 110, 96, 87, 83, 92, 100, 117, 140, 172, 208,
                254, 299, 354
            ]
        );
    }

    #[test]
    fn puts_label_0_nearest_the_origin() {
        assert_eq!(point(0), (1, 1));
        assert_eq!(point(415), (45, 9));
    }

    #[test]
    fn turns_clockwise() {
        assert_eq!(rotate((1, 1), 1), (1, -1));
        assert_eq!(rotate((1, 1), 2), (-1, -1));
        assert_eq!(rotate((5, 1), 4), (5, 1));
    }

    #[test]
    fn labels_every_point_of_all_four_quarters() {
        for label_in in 0..QUARTER {
            for turns in 0..4 {
                assert_eq!(
                    label(rotate(point(label_in), turns)),
                    Some((label_in, turns))
                );
            }
        }
        assert_eq!(label((49, 49)), None);
    }
}
