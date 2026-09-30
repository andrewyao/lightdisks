use std::collections::HashMap;

use crate::tree::{NodeId, Tree};

/// Folders taken out of the treemap. Hiding is view state over the scan: the
/// tree keeps its sizes, and layout reads them through `size`, so restoring or
/// trashing can never leave the two out of step.
#[derive(Default)]
pub struct Hidden {
    /// In the order they were hidden.
    ids: Vec<NodeId>,
    /// Bytes to subtract from each node, derived from `ids` by `rebuild`.
    removed: HashMap<NodeId, u64>,
}

impl Hidden {
    pub fn hide(&mut self, tree: &Tree, id: NodeId) {
        if !self.contains(id) {
            self.ids.push(id);
        }
        self.rebuild(tree);
    }

    pub fn restore(&mut self, tree: &Tree, id: NodeId) {
        self.ids.retain(|&h| h != id);
        self.rebuild(tree);
    }

    /// Call after `Tree::remove`. A trashed folder's descendants keep their
    /// sizes, so a hidden folder inside it must go before it is subtracted
    /// from the zeroed ancestor.
    pub fn sync(&mut self, tree: &Tree) {
        self.ids
            .retain(|&h| tree.ancestors(h).all(|a| tree.node(a).size > 0));
        self.rebuild(tree);
    }

    pub fn ids(&self) -> &[NodeId] {
        &self.ids
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.ids.contains(&id)
    }

    /// The bytes of `id` still shown: zero for a hidden folder, and less than
    /// its scanned size for an ancestor of one.
    pub fn size(&self, tree: &Tree, id: NodeId) -> u64 {
        tree.node(id).size - self.removed.get(&id).copied().unwrap_or(0)
    }

    fn rebuild(&mut self, tree: &Tree) {
        self.removed.clear();
        for &id in &self.ids {
            if tree.ancestors(id).skip(1).any(|a| self.contains(a)) {
                continue;
            }
            let size = tree.node(id).size;
            for a in tree.ancestors(id) {
                *self.removed.entry(a).or_default() += size;
            }
        }
    }
}
