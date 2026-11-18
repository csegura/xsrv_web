// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Regions as lists of rectangles that may overlap. Enough for window shapes.
use super::resources::{IRect, Surface};

pub fn union(a: &[IRect], b: &[IRect]) -> Vec<IRect> {
    a.iter().chain(b).copied().collect()
}

/// The parts that are in both regions. `None` when there would be more than `limit` rects.
pub fn intersect(a: &[IRect], b: &[IRect], limit: usize) -> Option<Vec<IRect>> {
    let mut out = Vec::new();
    for first in a {
        for second in b {
            if let Some(rect) = first.intersect(*second) {
                out.push(rect);
                if out.len() > limit {
                    return None;
                }
            }
        }
    }
    Some(out)
}

/// What is left of `rect` after `hole` is cut out: up to four rects that do not overlap.
fn cut(rect: IRect, hole: IRect) -> Vec<IRect> {
    let Some(inner) = rect.intersect(hole) else {
        return vec![rect];
    };
    [
        IRect {
            y1: inner.y0,
            ..rect
        },
        IRect {
            y0: inner.y1,
            ..rect
        },
        IRect {
            x1: inner.x0,
            y0: inner.y0,
            y1: inner.y1,
            ..rect
        },
        IRect {
            x0: inner.x1,
            y0: inner.y0,
            y1: inner.y1,
            ..rect
        },
    ]
    .into_iter()
    .filter(|part| part.x0 < part.x1 && part.y0 < part.y1)
    .collect()
}

/// `a` without `b`. `None` when there would be more than `limit` rects at any step.
pub fn subtract(a: &[IRect], b: &[IRect], limit: usize) -> Option<Vec<IRect>> {
    let mut left = a.to_vec();
    for hole in b {
        let mut next = Vec::with_capacity(left.len());
        for rect in left {
            next.extend(cut(rect, *hole));
            if next.len() > limit {
                return None;
            }
        }
        left = next;
    }
    Some(left)
}

/// The same area as rects that do not overlap, in bands of equal height. A region that is stored
/// this way costs one copy of each pixel when it is painted, whatever was done to build it.
pub fn normalize(rects: &[IRect]) -> Vec<IRect> {
    let real: Vec<IRect> = rects
        .iter()
        .copied()
        .filter(|r| r.x0 < r.x1 && r.y0 < r.y1)
        .collect();
    let mut ys: Vec<i32> = real.iter().flat_map(|r| [r.y0, r.y1]).collect();
    ys.sort_unstable();
    ys.dedup();
    let mut out: Vec<IRect> = Vec::new();
    for band in ys.windows(2) {
        let (top, bottom) = (band[0], band[1]);
        let mut spans: Vec<(i32, i32)> = real
            .iter()
            .filter(|r| r.y0 <= top && r.y1 >= bottom)
            .map(|r| (r.x0, r.x1))
            .collect();
        spans.sort_unstable();
        let mut merged: Vec<(i32, i32)> = Vec::new();
        for (x0, x1) in spans {
            match merged.last_mut() {
                Some(last) if x0 <= last.1 => last.1 = last.1.max(x1),
                _ => merged.push((x0, x1)),
            }
        }
        for (x0, x1) in merged {
            let above = out
                .iter_mut()
                .rev()
                .take_while(|p| p.y1 == top)
                .find(|p| p.x0 == x0 && p.x1 == x1);
            match above {
                Some(previous) => previous.y1 = bottom,
                None => out.push(IRect {
                    x0,
                    y0: top,
                    x1,
                    y1: bottom,
                }),
            }
        }
    }
    out
}

pub fn extents(region: &[IRect]) -> Option<IRect> {
    region.iter().copied().reduce(IRect::union)
}

pub fn contains(region: &[IRect], x: i32, y: i32) -> bool {
    region.iter().any(|rect| rect.contains(x, y))
}

pub fn translate(region: &[IRect], dx: i32, dy: i32) -> Vec<IRect> {
    region.iter().map(|rect| rect.translate(dx, dy)).collect()
}

/// The set pixels of a bitmap as rects: one for each run of set pixels in a row.
pub fn from_bitmap(bitmap: &Surface) -> Vec<IRect> {
    let bounds = bitmap.bounds();
    let mut rects = Vec::new();
    for y in bounds.y0..bounds.y1 {
        let row = bitmap.row(y, bounds.x0, bounds.x1);
        let mut x = 0;
        let pixels = row.as_chunks::<4>().0;
        while x < pixels.len() {
            if pixels[x][..3] == [0, 0, 0] {
                x += 1;
                continue;
            }
            let start = x;
            while x < pixels.len() && pixels[x][..3] != [0, 0, 0] {
                x += 1;
            }
            rects.push(IRect::new(start as i32, y, (x - start) as i32, 1));
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 1000;

    fn area(region: &[IRect]) -> i32 {
        // Only right for regions without overlap.
        region.iter().map(|r| r.width() * r.height()).sum()
    }

    #[test]
    fn subtracting_a_hole_leaves_a_frame() {
        let outer = IRect::new(0, 0, 10, 10);
        let left = subtract(&[outer], &[IRect::new(3, 3, 4, 4)], LIMIT).unwrap();
        assert_eq!(area(&left), 100 - 16);
        assert!(!contains(&left, 4, 4));
        assert!(contains(&left, 0, 0) && contains(&left, 9, 9) && contains(&left, 3, 2));
    }

    #[test]
    fn subtracting_a_disjoint_rect_changes_nothing() {
        let outer = IRect::new(0, 0, 4, 4);
        assert_eq!(
            subtract(&[outer], &[IRect::new(10, 10, 2, 2)], LIMIT),
            Some(vec![outer])
        );
        assert_eq!(subtract(&[outer], &[outer], LIMIT), Some(Vec::new()));
    }

    #[test]
    fn intersect_and_union() {
        let a = [IRect::new(0, 0, 6, 6)];
        let b = [IRect::new(4, 4, 6, 6)];
        assert_eq!(intersect(&a, &b, LIMIT), Some(vec![IRect::new(4, 4, 2, 2)]));
        assert_eq!(union(&a, &b).len(), 2);
        assert_eq!(
            intersect(&a, &[IRect::new(20, 20, 1, 1)], LIMIT),
            Some(Vec::new())
        );
    }

    #[test]
    fn operations_stop_at_the_limit_instead_of_growing() {
        let rows: Vec<IRect> = (0..100).map(|i| IRect::new(0, i, 100, 1)).collect();
        let columns: Vec<IRect> = (0..100).map(|i| IRect::new(i, 0, 1, 100)).collect();
        assert_eq!(intersect(&rows, &columns, 5000), None);
        assert!(intersect(&rows, &columns, 10_000).is_some());
        let comb: Vec<IRect> = (0..50).map(|i| IRect::new(i * 2, 0, 1, 100)).collect();
        assert_eq!(
            subtract(&rows, &comb, 150),
            None,
            "every row falls into many pieces"
        );
    }

    #[test]
    fn normalizing_removes_overlap_and_merges_bands() {
        let same = vec![IRect::new(0, 0, 10, 10); 500];
        assert_eq!(normalize(&same), vec![IRect::new(0, 0, 10, 10)]);
        let overlapping = [IRect::new(0, 0, 6, 6), IRect::new(4, 4, 6, 6)];
        assert_eq!(area(&normalize(&overlapping)), 36 + 36 - 4);
        let touching = [IRect::new(0, 0, 5, 3), IRect::new(0, 3, 5, 3)];
        assert_eq!(normalize(&touching), vec![IRect::new(0, 0, 5, 6)]);
        assert!(normalize(&[IRect::new(3, 3, 0, 5)]).is_empty());
    }

    #[test]
    fn extents_cover_everything() {
        let bounds = [IRect::new(0, 0, 2, 4), IRect::new(5, 1, 1, 1)];
        assert_eq!(extents(&bounds), Some(IRect::new(0, 0, 6, 4)));
        assert_eq!(extents(&[]), None);
    }

    #[test]
    fn bitmap_runs_become_rects() {
        let mut bitmap = Surface::new(5, 2);
        bitmap.fill_rect(IRect::new(1, 0, 2, 1), 0xFFFFFF, 3, u32::MAX);
        bitmap.fill_rect(IRect::new(4, 1, 1, 1), 0xFFFFFF, 3, u32::MAX);
        assert_eq!(
            from_bitmap(&bitmap),
            vec![IRect::new(1, 0, 2, 1), IRect::new(4, 1, 1, 1)]
        );
    }
}
