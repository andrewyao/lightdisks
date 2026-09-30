use std::path::PathBuf;

use lightdisks::layout::{
    FOLDER_PAD, FolderTile, Item, Rect, folders, items, layout, squarify,
};
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

enum Spec {
    Dir(&'static str, Vec<Spec>),
    File(&'static str, u64),
    Link(&'static str, u64),
}

impl Spec {
    fn size(&self) -> u64 {
        match self {
            Spec::Dir(_, kids) => kids.iter().map(Spec::size).sum(),
            Spec::File(_, size) | Spec::Link(_, size) => *size,
        }
    }
}

/// Numbers `spec` breadth-first, as the scanner does, so every directory's
/// children get one contiguous id range.
fn build(spec: Spec) -> Tree {
    let mut exts = ExtTable::default();
    let ext = exts.intern("bin");
    let mut nodes = Vec::new();
    let mut queue = std::collections::VecDeque::from([(spec, None::<u32>)]);
    let mut next = 1u32;
    while let Some((spec, parent)) = queue.pop_front() {
        let size = spec.size();
        let (name, kind) = match spec {
            Spec::Dir(name, kids) => {
                let start = next;
                next += kids.len() as u32;
                let id = nodes.len() as u32;
                queue.extend(kids.into_iter().map(|k| (k, Some(id))));
                (name, Kind::Dir { children: start..next })
            }
            Spec::File(name, _) => {
                exts.bytes[ext.0 as usize] += size;
                (name, Kind::File { ext })
            }
            Spec::Link(name, _) => (name, Kind::Other),
        };
        nodes.push(node(name, parent, size, kind));
    }
    Tree {
        nodes,
        exts,
        root_path: PathBuf::from("/root"),
        errors: 0,
    }
}

/// root/{big/{x/{p, q}, y/{r}, s, link}, mid/{m}, dirs_only/{z/{t}},
/// solo/{inner/{u}, v}, a, b}
fn folder_tree() -> Tree {
    use Spec::*;
    build(Dir(
        "root",
        vec![
            Dir(
                "big",
                vec![
                    Dir("x", vec![File("p", 300_000), File("q", 100_000)]),
                    Dir("y", vec![File("r", 200_000)]),
                    File("s", 150_000),
                    Link("link", 50_000),
                ],
            ),
            Dir("mid", vec![File("m", 400_000)]),
            Dir("dirs_only", vec![Dir("z", vec![File("t", 250_000)])]),
            Dir(
                "solo",
                vec![Dir("inner", vec![File("u", 180_000)]), File("v", 90_000)],
            ),
            File("a", 120_000),
            File("b", 80_000),
        ],
    ))
}

fn id(tree: &Tree, path: &str) -> NodeId {
    path.split('/')
        .fold(Tree::ROOT, |dir, name| tree.find_child(dir, name).unwrap())
}

const BIG: Rect = Rect {
    x: 5.0,
    y: 7.0,
    w: 1600.0,
    h: 1200.0,
};

/// Index of each tile's parent tile: the nearest earlier tile one level up.
fn parents(tiles: &[FolderTile]) -> Vec<Option<usize>> {
    tiles
        .iter()
        .enumerate()
        .map(|(i, t)| (0..i).rev().find(|&j| tiles[j].depth + 1 == t.depth))
        .collect()
}

#[test]
fn folder_tiles_stop_at_max_depth() {
    let tree = folder_tree();
    for max_depth in 1..=4 {
        let tiles = folders(&tree, Tree::ROOT, BIG, max_depth, 1.0);
        assert!(
            tiles.iter().all(|t| (1..=max_depth).contains(&t.depth)),
            "max_depth {max_depth}: {:?}",
            tiles.iter().map(|t| t.depth).collect::<Vec<_>>()
        );
        assert!(
            tiles
                .iter()
                .filter(|t| t.depth == max_depth)
                .all(|t| t.header.is_none()),
            "a folder at max_depth {max_depth} was opened"
        );
    }
    let deepest = folders(&tree, Tree::ROOT, BIG, 4, 1.0)
        .iter()
        .map(|t| t.depth)
        .max();
    assert_eq!(deepest, Some(3), "the fixture is three folders deep");
}

#[test]
fn folder_children_sit_inside_the_parent_below_its_header() {
    let tree = folder_tree();
    let tiles = folders(&tree, Tree::ROOT, BIG, 4, 1.0);
    let eps = 1e-2;
    for (tile, parent) in tiles.iter().zip(parents(&tiles)) {
        let r = tile.rect;
        let Some(p) = parent else {
            assert_eq!(tile.depth, 1);
            assert_sane(&r, BIG);
            continue;
        };
        let p = &tiles[p];
        let header = p.header.expect("a folder with child tiles has a header");
        assert_eq!(
            (header.x, header.y, header.w),
            (p.rect.x, p.rect.y, p.rect.w)
        );
        assert!(header.h >= 12.0, "header too short for a label: {header:?}");
        assert!(
            r.y >= header.y + header.h - eps,
            "{r:?} overlaps header {header:?}"
        );
        assert!(
            r.x >= p.rect.x + FOLDER_PAD - eps,
            "{r:?} in left padding of {:?}",
            p.rect
        );
        assert!(
            r.x + r.w <= p.rect.x + p.rect.w - FOLDER_PAD + eps,
            "{r:?} in right padding of {:?}",
            p.rect
        );
        assert!(
            r.y + r.h <= p.rect.y + p.rect.h - FOLDER_PAD + eps,
            "{r:?} in bottom padding of {:?}",
            p.rect
        );
    }
}

#[test]
fn sibling_folder_areas_are_proportional_to_their_bytes() {
    let tree = folder_tree();
    let tiles = folders(&tree, Tree::ROOT, BIG, 4, 1.0);
    let parents = parents(&tiles);
    let root_bytes = tree.node(Tree::ROOT).size as f32;
    for t in tiles.iter().filter(|t| t.depth == 1) {
        let expected = BIG.area() * t.item.bytes(&tree) as f32 / root_bytes;
        assert!(
            (t.rect.area() - expected).abs() <= BIG.area() * 1e-4,
            "{:?}: area {}, expected {expected}",
            t.item,
            t.rect.area()
        );
    }
    let mut checked = 0;
    for (i, a) in tiles.iter().enumerate() {
        for (j, b) in tiles.iter().enumerate().skip(i + 1) {
            if parents[i] != parents[j] {
                continue;
            }
            let per_byte_a = a.rect.area() / a.item.bytes(&tree) as f32;
            let per_byte_b = b.rect.area() / b.item.bytes(&tree) as f32;
            assert!(
                (per_byte_a / per_byte_b - 1.0).abs() < 1e-3,
                "{:?} and {:?} are not in proportion",
                a.item,
                b.item
            );
            checked += 1;
        }
    }
    assert!(checked > 10, "only {checked} sibling pairs");
}

#[test]
fn a_files_tile_appears_exactly_when_a_folder_has_direct_files() {
    let mut tree = folder_tree();
    tree.remove(id(&tree, "solo/v"));
    let tiles = folders(&tree, Tree::ROOT, BIG, 4, 0.0);

    let files_of = |dir: NodeId| {
        let found: Vec<u64> = tiles
            .iter()
            .filter_map(|t| match t.item {
                Item::Files { dir: d, bytes } if d == dir => Some(bytes),
                _ => None,
            })
            .collect();
        assert!(found.len() <= 1, "{dir:?} has {} files tiles", found.len());
        found.first().copied()
    };
    let mut dirs = vec![Tree::ROOT];
    dirs.extend(
        tiles
            .iter()
            .filter(|t| t.header.is_some())
            .map(|t| t.item.node()),
    );
    assert_eq!(dirs.len(), 9, "every folder is opened");
    for dir in dirs {
        let direct: u64 = tree
            .children(dir)
            .filter(|&c| !tree.is_dir(c))
            .map(|c| tree.node(c).size)
            .sum();
        let expected = (direct > 0).then_some(direct);
        assert_eq!(files_of(dir), expected, "{}", tree.path(dir).display());
    }
    assert_eq!(files_of(id(&tree, "big")), Some(200_000), "file plus symlink");
    assert_eq!(files_of(Tree::ROOT), Some(200_000));
    assert_eq!(files_of(id(&tree, "dirs_only")), None);
    assert_eq!(files_of(id(&tree, "solo")), None, "its only file was trashed");
}

#[test]
fn a_trashed_folder_gets_no_tile() {
    let mut tree = folder_tree();
    let mid = id(&tree, "mid");
    tree.remove(mid);
    let tiles = folders(&tree, Tree::ROOT, BIG, 4, 0.0);
    assert!(tiles.iter().all(|t| t.item != Item::Node(mid)));
    assert!(
        items(&tree, Tree::ROOT)
            .iter()
            .all(|&(item, _)| item != Item::Node(mid))
    );
}
