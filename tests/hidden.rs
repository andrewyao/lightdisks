mod common;

use common::{Spec, build, id};
use lightdisks::hidden::Hidden;
use lightdisks::layout::{Item, Rect, folders, items, layout};
use lightdisks::tree::Tree;

/// root/{big/{x/{p, q}, y/{r}, s}, mid/{m}, small/{t}, a}
fn tree() -> Tree {
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
                ],
            ),
            Dir("mid", vec![File("m", 400_000)]),
            Dir("small", vec![File("t", 50_000)]),
            File("a", 100_000),
        ],
    ))
}

const BOUNDS: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 800.0,
    h: 600.0,
};

fn snapshot(tree: &Tree, hidden: &Hidden) -> String {
    format!(
        "{:?}\n{:?}",
        folders(tree, hidden, Tree::ROOT, BOUNDS, 4, 0.0),
        layout(tree, hidden, Tree::ROOT, BOUNDS, 1.0)
    )
}

#[test]
fn a_hidden_folder_leaves_the_treemap_and_its_siblings_fill_the_rect() {
    let tree = tree();
    let big = id(&tree, "big");
    let mid = id(&tree, "mid");
    let mut hidden = Hidden::default();
    hidden.hide(&tree, big);

    assert!(
        items(&tree, &hidden, Tree::ROOT)
            .iter()
            .all(|&(item, _)| item != Item::Node(big))
    );
    let tiles = folders(&tree, &hidden, Tree::ROOT, BOUNDS, 4, 0.0);
    assert!(tiles.iter().all(|t| !tree.is_within(t.item.node(), big)));
    let top: Vec<_> = tiles.iter().filter(|t| t.depth == 1).collect();
    let covered: f32 = top.iter().map(|t| t.rect.area()).sum();
    assert!(
        (covered - BOUNDS.area()).abs() < 1.0,
        "visible folders cover {covered} of {}",
        BOUNDS.area()
    );
    let mid_tile = top.iter().find(|t| t.item == Item::Node(mid)).unwrap();
    let expected = BOUNDS.area() * 400_000.0 / 550_000.0;
    assert!((mid_tile.rect.area() - expected).abs() < 1.0);

    let tiles = layout(&tree, &hidden, Tree::ROOT, BOUNDS, 1.0);
    assert!(tiles.iter().all(|t| !tree.is_within(t.id, big)));
    let mid_tile = tiles.iter().find(|t| t.id == mid).unwrap();
    assert!((mid_tile.rect.area() - expected).abs() < 1.0);
}

#[test]
fn a_hidden_grandchild_shrinks_its_parent_against_the_parents_sibling() {
    let tree = tree();
    let mut hidden = Hidden::default();
    hidden.hide(&tree, id(&tree, "big/x"));

    let tiles = folders(&tree, &hidden, Tree::ROOT, BOUNDS, 1, 0.0);
    let area = |path: &str| {
        let node = id(&tree, path);
        tiles
            .iter()
            .find(|t| t.item == Item::Node(node))
            .unwrap()
            .rect
            .area()
    };
    let ratio = area("big") / area("mid");
    assert!(
        (ratio - 350_000.0 / 400_000.0).abs() < 1e-3,
        "big/mid is {ratio}"
    );
    assert_eq!(hidden.size(&tree, Tree::ROOT), 900_000);
}

#[test]
fn restoring_gives_back_the_original_tiles() {
    let tree = tree();
    let original = snapshot(&tree, &Hidden::default());
    let mut hidden = Hidden::default();
    for path in ["big", "big/y", "small"] {
        hidden.hide(&tree, id(&tree, path));
    }
    assert_ne!(snapshot(&tree, &hidden), original);
    for path in ["big/y", "small", "big"] {
        hidden.restore(&tree, id(&tree, path));
    }
    assert!(hidden.ids().is_empty());
    assert_eq!(snapshot(&tree, &hidden), original);
}

#[test]
fn restoring_a_child_of_a_hidden_folder_keeps_the_folder_hidden() {
    let tree = tree();
    let big = id(&tree, "big");
    let x = id(&tree, "big/x");
    let mut hidden = Hidden::default();
    hidden.hide(&tree, x);
    hidden.hide(&tree, big);
    assert_eq!(hidden.size(&tree, Tree::ROOT), 550_000, "counted once");

    hidden.restore(&tree, x);
    assert_eq!(hidden.ids(), [big]);
    assert_eq!(hidden.size(&tree, Tree::ROOT), 550_000);
    assert!(
        items(&tree, &hidden, Tree::ROOT)
            .iter()
            .all(|&(item, _)| item != Item::Node(big))
    );
}

#[test]
fn trashing_a_folder_drops_the_hidden_folders_inside_it() {
    let mut tree = tree();
    let big = id(&tree, "big");
    let small = id(&tree, "small");
    let mut hidden = Hidden::default();
    hidden.hide(&tree, id(&tree, "big/x"));
    hidden.hide(&tree, small);

    tree.remove(big);
    hidden.sync(&tree);

    assert_eq!(hidden.ids(), [small]);
    assert_eq!(hidden.size(&tree, big), 0);
    assert_eq!(hidden.size(&tree, Tree::ROOT), 500_000);
    let tiles = folders(&tree, &hidden, Tree::ROOT, BOUNDS, 4, 0.0);
    assert!(tiles.iter().all(|t| t.item.node() != big));
}
