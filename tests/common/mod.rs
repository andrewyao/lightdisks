#![allow(dead_code)]

use std::path::PathBuf;

use lightdisks::tree::{ExtTable, Kind, Node, NodeId, Tree};

pub fn node(name: &str, parent: Option<u32>, size: u64, kind: Kind) -> Node {
    Node {
        name: name.into(),
        parent: parent.map(NodeId),
        size,
        kind,
    }
}

pub enum Spec {
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
pub fn build(spec: Spec) -> Tree {
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
                (
                    name,
                    Kind::Dir {
                        children: start..next,
                    },
                )
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

pub fn id(tree: &Tree, path: &str) -> NodeId {
    path.split('/')
        .fold(Tree::ROOT, |dir, name| tree.find_child(dir, name).unwrap())
}
