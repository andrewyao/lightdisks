use std::fs;
use std::ops::ControlFlow;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use lightdisks::scan::{self, ScanMsg};
use lightdisks::tree::{Kind, NodeId, Tree};

fn allocated(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().blocks() * 512
}

fn write(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; len]).unwrap();
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("tiny.txt"), 1);
        write(&r.join("big.bin"), 300_000);
        write(&r.join("sub/notes.txt"), 5_000);
        write(&r.join("sub/nested/data.dat"), 40_000);
        fs::hard_link(r.join("big.bin"), r.join("big-hardlink.bin")).unwrap();
        std::os::unix::fs::symlink(r.join("big.bin"), r.join("big-symlink")).unwrap();
        std::os::unix::fs::symlink(r.join("sub"), r.join("sub-symlink")).unwrap();
        Fixture { dir }
    }

    fn path(&self, rel: &str) -> std::path::PathBuf {
        self.dir.path().join(rel)
    }

    fn scan(&self) -> Tree {
        scan::scan(self.dir.path(), |_| ControlFlow::Continue(())).unwrap()
    }
}

fn child(tree: &Tree, parent: NodeId, name: &str) -> NodeId {
    tree.find_child(parent, name)
        .unwrap_or_else(|| panic!("{name} missing under {}", tree.path(parent).display()))
}

#[test]
fn root_total_counts_allocated_blocks_once_per_inode() {
    let fx = Fixture::new();
    let tree = fx.scan();
    let expected = allocated(&fx.path("tiny.txt"))
        + allocated(&fx.path("big.bin"))
        + allocated(&fx.path("sub/notes.txt"))
        + allocated(&fx.path("sub/nested/data.dat"))
        + allocated(&fx.path("big-symlink"))
        + allocated(&fx.path("sub-symlink"));
    assert_eq!(tree.node(Tree::ROOT).size, expected);
    assert_eq!(tree.errors, 0);
}

#[test]
fn file_size_is_allocated_not_logical() {
    let fx = Fixture::new();
    let tree = fx.scan();
    let tiny = tree.node(child(&tree, Tree::ROOT, "tiny.txt"));
    assert_eq!(tiny.size, allocated(&fx.path("tiny.txt")));
    assert!(
        tiny.size >= 512,
        "a 1-byte file occupies at least one block, got {}",
        tiny.size
    );
}

#[test]
fn hard_link_pair_is_counted_once() {
    let fx = Fixture::new();
    let tree = fx.scan();
    let a = tree.node(child(&tree, Tree::ROOT, "big.bin")).size;
    let b = tree.node(child(&tree, Tree::ROOT, "big-hardlink.bin")).size;
    let big = allocated(&fx.path("big.bin"));
    assert!(big >= 300_000);
    assert_eq!(a + b, big, "sizes {a} and {b} should sum to one copy");
    assert!(a == 0 || b == 0);
}

#[test]
fn symlinks_are_not_followed() {
    let fx = Fixture::new();
    let tree = fx.scan();
    let file_link = tree.node(child(&tree, Tree::ROOT, "big-symlink"));
    assert!(matches!(file_link.kind, Kind::Other));
    assert_eq!(file_link.size, allocated(&fx.path("big-symlink")));
    assert!(file_link.size < allocated(&fx.path("big.bin")));

    let dir_link = child(&tree, Tree::ROOT, "sub-symlink");
    assert!(matches!(tree.node(dir_link).kind, Kind::Other));
    assert_eq!(tree.children(dir_link).count(), 0);
}

#[test]
fn directory_sizes_equal_the_sum_of_their_children() {
    let fx = Fixture::new();
    let tree = fx.scan();
    let sub = child(&tree, Tree::ROOT, "sub");
    let nested = child(&tree, sub, "nested");
    assert_eq!(
        tree.node(nested).size,
        allocated(&fx.path("sub/nested/data.dat"))
    );
    assert_eq!(
        tree.node(sub).size,
        allocated(&fx.path("sub/notes.txt")) + allocated(&fx.path("sub/nested/data.dat"))
    );
    for id in (0..tree.nodes.len() as u32).map(NodeId) {
        if tree.is_dir(id) {
            let sum: u64 = tree.children(id).map(|c| tree.node(c).size).sum();
            assert_eq!(tree.node(id).size, sum, "{}", tree.path(id).display());
        }
    }
}

#[test]
fn children_are_sorted_by_size_descending_and_point_back_to_parent() {
    let fx = Fixture::new();
    let tree = fx.scan();
    for id in (0..tree.nodes.len() as u32).map(NodeId) {
        let sizes: Vec<u64> = tree.children(id).map(|c| tree.node(c).size).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
        assert!(tree.children(id).all(|c| tree.node(c).parent == Some(id)));
    }
    assert_eq!(
        tree.path(child(&tree, child(&tree, Tree::ROOT, "sub"), "nested")),
        fx.path("sub/nested")
    );
}

#[test]
fn extension_totals_track_file_bytes() {
    let fx = Fixture::new();
    let tree = fx.scan();
    let txt = tree.exts.index["txt"];
    assert_eq!(
        tree.exts.bytes[txt.0 as usize],
        allocated(&fx.path("tiny.txt")) + allocated(&fx.path("sub/notes.txt"))
    );
}

#[test]
fn remove_subtracts_up_the_parent_chain() {
    let fx = Fixture::new();
    let mut tree = fx.scan();
    let root_before = tree.node(Tree::ROOT).size;
    let sub = child(&tree, Tree::ROOT, "sub");
    let nested = child(&tree, sub, "nested");
    let nested_size = tree.node(nested).size;
    let dat = tree.exts.index["dat"];

    tree.remove(nested);

    assert_eq!(tree.node(nested).size, 0);
    assert_eq!(tree.node(sub).size, allocated(&fx.path("sub/notes.txt")));
    assert_eq!(tree.node(Tree::ROOT).size, root_before - nested_size);
    assert_eq!(tree.exts.bytes[dat.0 as usize], 0);
}

#[test]
fn background_scan_ends_with_done() {
    let fx = Fixture::new();
    let rx = scan::spawn(fx.dir.path().to_path_buf(), || {});
    let tree = rx
        .iter()
        .find_map(|msg| match msg {
            ScanMsg::Done(tree) => Some(tree),
            ScanMsg::Failed(e) => panic!("{e}"),
            ScanMsg::Progress(_) => None,
        })
        .expect("scan thread hung up without a result");
    assert_eq!(tree.node(Tree::ROOT).size, fx.scan().node(Tree::ROOT).size);
}

#[test]
fn scanning_a_file_fails() {
    let fx = Fixture::new();
    assert!(scan::scan(&fx.path("big.bin"), |_| ControlFlow::Continue(())).is_err());
}

#[test]
fn child_toward_finds_the_zoom_target() {
    let fx = Fixture::new();
    let tree = fx.scan();
    let sub = child(&tree, Tree::ROOT, "sub");
    let nested = child(&tree, sub, "nested");
    let data = child(&tree, nested, "data.dat");
    assert_eq!(tree.child_toward(Tree::ROOT, data), Some(sub));
    assert_eq!(tree.child_toward(sub, data), Some(nested));
    assert_eq!(tree.child_toward(sub, sub), None);
    assert_eq!(tree.child_toward(nested, sub), None);
}
