use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ExtId(pub u32);

#[derive(Debug)]
pub enum Kind {
    Dir { children: Range<u32> },
    File { ext: ExtId },
    Other,
}

#[derive(Debug)]
pub struct Node {
    pub name: Box<str>,
    pub parent: Option<NodeId>,
    pub size: u64,
    pub kind: Kind,
}

/// Arena of scanned entries. Node 0 is the root. A directory's children occupy
/// one contiguous id range, sorted by size descending as of the scan.
#[derive(Debug)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub exts: ExtTable,
    pub root_path: PathBuf,
    pub errors: u64,
}

/// Interned lowercase file extensions with their total bytes. The empty name
/// stands for "no extension".
#[derive(Debug, Default)]
pub struct ExtTable {
    pub names: Vec<Box<str>>,
    pub index: HashMap<Box<str>, ExtId>,
    pub bytes: Vec<u64>,
}

impl ExtTable {
    pub fn intern(&mut self, name: &str) -> ExtId {
        if let Some(&id) = self.index.get(name) {
            return id;
        }
        let id = ExtId(self.names.len() as u32);
        self.names.push(name.into());
        self.index.insert(name.into(), id);
        self.bytes.push(0);
        id
    }

    pub fn name(&self, id: ExtId) -> &str {
        &self.names[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Lowercased extension of a file name; dotfiles like `.zshrc` have none.
pub fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

impl Tree {
    pub const ROOT: NodeId = NodeId(0);

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn children(&self, id: NodeId) -> impl DoubleEndedIterator<Item = NodeId> + '_ {
        let range = match &self.node(id).kind {
            Kind::Dir { children } => children.clone(),
            _ => 0..0,
        };
        range.map(NodeId)
    }

    pub fn is_dir(&self, id: NodeId) -> bool {
        matches!(self.node(id).kind, Kind::Dir { .. })
    }

    /// `id` followed by its parent, grandparent, and so on up to the root.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(id), |&n| self.node(n).parent)
    }

    pub fn is_within(&self, id: NodeId, ancestor: NodeId) -> bool {
        self.ancestors(id).any(|a| a == ancestor)
    }

    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut names: Vec<&str> = self
            .ancestors(id)
            .take_while(|&n| n != Self::ROOT)
            .map(|n| &*self.node(n).name)
            .collect();
        names.reverse();
        let mut path = self.root_path.clone();
        path.extend(names.iter().map(Path::new));
        path
    }

    pub fn find_child(&self, id: NodeId, name: &str) -> Option<NodeId> {
        self.children(id).find(|&c| &*self.node(c).name == name)
    }

    /// Forget a subtree that no longer exists on disk: its bytes leave every
    /// ancestor and the extension totals, and a zero size keeps it out of layout.
    pub fn remove(&mut self, id: NodeId) {
        let size = self.node(id).size;
        if size == 0 {
            return;
        }
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let node = &self.nodes[n.0 as usize];
            match &node.kind {
                Kind::File { ext } => self.exts.bytes[ext.0 as usize] -= node.size,
                Kind::Dir { children } => stack.extend(children.clone().map(NodeId)),
                Kind::Other => {}
            }
        }
        let ancestors: Vec<NodeId> = self.ancestors(id).collect();
        for a in ancestors {
            self.nodes[a.0 as usize].size -= size;
        }
    }
}
