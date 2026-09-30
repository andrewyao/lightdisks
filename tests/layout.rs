use std::path::PathBuf;

use lightdisks::layout::{Rect, layout, squarify};
use lightdisks::tree::{ExtTable, Kind, Node, NodeId, Tree};

fn rect(w: f32, h: f32) -> Rect {
    Rect {
        x: 10.0,
        y: 20.0,
        w,
        h,
    }
}

fn assert_sane(r: &Rect, bounds: Rect) {
    for v in [r.x, r.y, r.w, r.h] {
        assert!(v.is_finite(), "{r:?}");
    }
    assert!(r.w >= 0.0 && r.h >= 0.0, "{r:?}");
    let eps = 1e-2;
    assert!(
        r.x >= bounds.x - eps && r.y >= bounds.y - eps,
        "{r:?} outside {bounds:?}"
    );
    assert!(
        r.x + r.w <= bounds.x + bounds.w + eps,
        "{r:?} outside {bounds:?}"
    );
    assert!(
        r.y + r.h <= bounds.y + bounds.h + eps,
        "{r:?} outside {bounds:?}"
    );
}

fn overlap(a: &Rect, b: &Rect) -> f32 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    w.max(0.0) * h.max(0.0)
}

const CASES: &[&[u64]] = &[
    &[1],
    &[6, 6, 4, 3, 2, 2, 1],
    &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
    &[1_000_000_000, 1, 1, 1],
    &[7; 40],
    &[40, 0, 30, 0, 20, 10],
    &[3, 2, 0],
];

#[test]
fn tiles_cover_the_rect_in_proportion_to_their_sizes() {
    for bounds in [rect(600.0, 400.0), rect(50.0, 900.0), rect(333.3, 333.3)] {
        for sizes in CASES {
            let total: u64 = sizes.iter().sum();
            let tiles = squarify(sizes, total, bounds);
            assert_eq!(tiles.len(), sizes.len());

            let sum: f32 = tiles.iter().map(Rect::area).sum();
            assert!(
                (sum - bounds.area()).abs() <= bounds.area() * 1e-4,
                "{sizes:?}: areas sum to {sum}, rect is {}",
                bounds.area()
            );
            for (r, &size) in tiles.iter().zip(sizes.iter()) {
                assert_sane(r, bounds);
                let expected = bounds.area() * size as f32 / total as f32;
                assert!(
                    (r.area() - expected).abs() <= bounds.area() * 1e-4,
                    "{sizes:?}: size {size} got area {}, expected {expected}",
                    r.area()
                );
            }
            for (i, a) in tiles.iter().enumerate() {
                for b in &tiles[i + 1..] {
                    assert!(overlap(a, b) < 1e-2, "{sizes:?}: {a:?} overlaps {b:?}");
                }
            }
        }
    }
}

#[test]
fn a_partial_total_leaves_the_remainder_uncovered() {
    let bounds = rect(200.0, 100.0);
    let tiles = squarify(&[30, 20], 100, bounds);
    let sum: f32 = tiles.iter().map(Rect::area).sum();
    assert!((sum - bounds.area() / 2.0).abs() < 1e-2, "covered {sum}");
    tiles.iter().for_each(|r| assert_sane(r, bounds));
}

#[test]
fn degenerate_inputs_yield_empty_tiles_not_nan() {
    for (sizes, total, bounds) in [
        (&[0u64, 0][..], 0, rect(100.0, 100.0)),
        (&[5, 5][..], 10, rect(0.0, 100.0)),
        (&[5, 5][..], 10, rect(100.0, 0.0)),
        (&[0, 3][..], 3, rect(100.0, 100.0)),
    ] {
        for r in squarify(sizes, total, bounds) {
            assert_sane(&r, bounds);
        }
    }
}

#[test]
fn equal_sizes_produce_near_square_tiles() {
    let tiles = squarify(&[1; 16], 16, rect(400.0, 400.0));
    for r in tiles {
        let aspect = r.w.max(r.h) / r.w.min(r.h);
        assert!(aspect < 2.0, "sliver {r:?}");
    }
}

fn node(name: &str, parent: Option<u32>, size: u64, kind: Kind) -> Node {
    Node {
        name: name.into(),
        parent: parent.map(NodeId),
        size,
        kind,
    }
}

/// root/{a/{d.bin, e.bin}, b.txt, c.txt} where c.txt is far below one pixel.
fn sample_tree() -> Tree {
    let mut exts = ExtTable::default();
    let txt = exts.intern("txt");
    let bin = exts.intern("bin");
    exts.bytes = vec![400_000, 600_000];
    Tree {
        nodes: vec![
            node("root", None, 1_000_000, Kind::Dir { children: 1..4 }),
            node("a", Some(0), 600_000, Kind::Dir { children: 4..6 }),
            node("b.txt", Some(0), 399_990, Kind::File { ext: txt }),
            node("c.txt", Some(0), 10, Kind::File { ext: txt }),
            node("d.bin", Some(1), 500_000, Kind::File { ext: bin }),
            node("e.bin", Some(1), 100_000, Kind::File { ext: bin }),
        ],
        exts,
        root_path: PathBuf::from("/root"),
        errors: 0,
    }
}

#[test]
fn layout_nests_children_inside_parents_and_drops_subpixel_tiles() {
    let tree = sample_tree();
    let bounds = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let tiles = layout(&tree, Tree::ROOT, bounds, 2.0);

    let ids: Vec<u32> = tiles.iter().map(|t| t.id.0).collect();
    assert_eq!(ids, vec![0, 1, 4, 5, 2], "pre-order without c.txt");

    let find = |id: u32| tiles.iter().find(|t| t.id.0 == id).unwrap();
    let a = find(1).rect;
    for child in [4, 5] {
        let r = find(child).rect;
        assert_sane(&r, a);
        assert_eq!(find(child).depth, 2);
    }
    assert!((find(4).rect.area() - 5_000.0).abs() < 0.5);
    assert!((find(2).rect.area() - 3_999.9).abs() < 0.5);
}

#[test]
fn layout_skips_removed_nodes() {
    let mut tree = sample_tree();
    tree.remove(NodeId(4));
    let tiles = layout(
        &tree,
        Tree::ROOT,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        },
        2.0,
    );
    assert!(tiles.iter().all(|t| t.id != NodeId(4)));
    let e = tiles.iter().find(|t| t.id == NodeId(5)).unwrap();
    assert!((e.rect.area() - 100_000.0 / 500_000.0 * 10_000.0).abs() < 0.5);
}
