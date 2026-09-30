use std::cmp::Reverse;

use crate::tree::{NodeId, Tree};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn area(&self) -> f32 {
        self.w * self.h
    }
}

/// One laid-out node. `cushion` holds the coefficients of the Van Wijk surface
/// `z = c[0]x² + c[1]x + c[2]y² + c[3]y` accumulated from every ancestor.
#[derive(Clone, Debug)]
pub struct Tile {
    pub id: NodeId,
    pub rect: Rect,
    pub cushion: [f32; 4],
    pub depth: u16,
}

const ROOT_HEIGHT: f32 = 0.38;
const HEIGHT_FALLOFF: f32 = 0.91;

/// Tiles for `root` and its descendants in pre-order, so a child always comes
/// after its parent and covers part of it. Children whose tile would be smaller
/// than `min_px`² are dropped, and directories narrower than `min_px` are not
/// opened.
pub fn layout(tree: &Tree, root: NodeId, rect: Rect, min_px: f32) -> Vec<Tile> {
    let mut cushion = [0.0; 4];
    add_ridges(&mut cushion, rect, ROOT_HEIGHT);
    let mut tiles = vec![Tile {
        id: root,
        rect,
        cushion,
        depth: 0,
    }];
    lay_children(
        tree,
        root,
        0,
        ROOT_HEIGHT * HEIGHT_FALLOFF,
        min_px,
        &mut tiles,
    );
    tiles
}

fn lay_children(
    tree: &Tree,
    dir: NodeId,
    parent: usize,
    height: f32,
    min_px: f32,
    tiles: &mut Vec<Tile>,
) {
    let Tile {
        rect,
        cushion,
        depth,
        ..
    } = tiles[parent].clone();
    let total = tree.node(dir).size;
    if total == 0 {
        return;
    }
    let px_per_byte = rect.area() as f64 / total as f64;
    let min_area = (min_px * min_px) as f64;

    let mut kids: Vec<(NodeId, u64)> = tree.children(dir).map(|c| (c, tree.node(c).size)).collect();
    // Trashing shrinks ancestors after the scan sorted them, so re-sort here.
    kids.sort_by_key(|&(_, size)| Reverse(size));
    let visible = kids.partition_point(|&(_, size)| size as f64 * px_per_byte >= min_area);
    kids.truncate(visible);

    let sizes: Vec<u64> = kids.iter().map(|&(_, size)| size).collect();
    for ((id, _), r) in kids.into_iter().zip(squarify(&sizes, total, rect)) {
        let mut c = cushion;
        add_ridges(&mut c, r, height);
        tiles.push(Tile {
            id,
            rect: r,
            cushion: c,
            depth: depth + 1,
        });
        if tree.is_dir(id) && r.w >= min_px && r.h >= min_px {
            let index = tiles.len() - 1;
            lay_children(tree, id, index, height * HEIGHT_FALLOFF, min_px, tiles);
        }
    }
}

/// Van Wijk & van de Wetering: a parabolic ridge across the rect in each axis,
/// whose edge slope is `4 * height` regardless of the rect's size.
fn add_ridges(c: &mut [f32; 4], r: Rect, height: f32) {
    if r.w > 0.0 {
        c[0] -= 4.0 * height / r.w;
        c[1] += 4.0 * height * (2.0 * r.x + r.w) / r.w;
    }
    if r.h > 0.0 {
        c[2] -= 4.0 * height / r.h;
        c[3] += 4.0 * height * (2.0 * r.y + r.h) / r.h;
    }
}

/// Squarified treemap (Bruls, Huizing, van Wijk). `rect` stands for `total`
/// bytes; each size gets a proportional tile. When `sizes` sum to less than
/// `total`, the remainder of `rect` is left uncovered. Zero sizes get an empty
/// rect. Sizes are best given in descending order.
pub fn squarify(sizes: &[u64], total: u64, rect: Rect) -> Vec<Rect> {
    let mut out = vec![
        Rect {
            x: rect.x,
            y: rect.y,
            w: 0.0,
            h: 0.0
        };
        sizes.len()
    ];
    if total == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return out;
    }
    let scale = rect.w as f64 * rect.h as f64 / total as f64;
    let areas: Vec<f64> = sizes.iter().map(|&s| s as f64 * scale).collect();
    let (mut x, mut y, mut w, mut h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);

    let mut i = 0;
    while i < areas.len() {
        let side = w.min(h);
        if areas[i] == 0.0 || side <= 0.0 {
            out[i] = Rect {
                x: x as f32,
                y: y as f32,
                w: 0.0,
                h: 0.0,
            };
            i += 1;
            continue;
        }
        let (mut sum, mut lo, mut hi) = (areas[i], areas[i], areas[i]);
        let mut end = i + 1;
        while end < areas.len() && areas[end] > 0.0 {
            let a = areas[end];
            if worst_ratio(sum + a, lo.min(a), hi.max(a), side) > worst_ratio(sum, lo, hi, side) {
                break;
            }
            (sum, lo, hi) = (sum + a, lo.min(a), hi.max(a));
            end += 1;
        }

        let thickness = sum / side;
        let mut offset = 0.0;
        for k in i..end {
            let length = areas[k] / thickness;
            out[k] = if w >= h {
                Rect {
                    x: x as f32,
                    y: (y + offset) as f32,
                    w: thickness as f32,
                    h: length as f32,
                }
            } else {
                Rect {
                    x: (x + offset) as f32,
                    y: y as f32,
                    w: length as f32,
                    h: thickness as f32,
                }
            };
            offset += length;
        }
        if w >= h {
            x += thickness;
            w = (w - thickness).max(0.0);
        } else {
            y += thickness;
            h = (h - thickness).max(0.0);
        }
        i = end;
    }
    out
}

/// The worst aspect ratio among a row of areas laid along `side`.
fn worst_ratio(sum: f64, lo: f64, hi: f64, side: f64) -> f64 {
    let side2 = side * side;
    let sum2 = sum * sum;
    (side2 * hi / sum2).max(sum2 / (side2 * lo))
}
