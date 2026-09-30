use std::cmp::Reverse;

use crate::tree::{ExtId, ExtTable, Kind, Node};

pub type Rgb = [u8; 3];

pub const DISTINCT: usize = 16;
pub const OTHER: Rgb = [150, 150, 150];
pub const FOLDER: Rgb = [110, 110, 116];

/// Chosen to stay distinguishable from each other and from `OTHER` once the
/// cushion shading darkens their edges.
const HUES: [Rgb; DISTINCT] = [
    [66, 133, 244],
    [234, 67, 53],
    [251, 188, 5],
    [52, 168, 83],
    [171, 71, 188],
    [0, 172, 193],
    [255, 112, 67],
    [158, 157, 36],
    [92, 107, 192],
    [240, 98, 146],
    [0, 137, 123],
    [255, 167, 38],
    [121, 85, 72],
    [124, 179, 66],
    [41, 182, 246],
    [186, 104, 200],
];

/// Colors fixed at scan time: the extensions with the most bytes get distinct
/// hues and keep them even as trashing changes the totals.
pub struct Palette {
    colors: Vec<Rgb>,
    pub ranked: Vec<ExtId>,
}

impl Palette {
    pub fn new(exts: &ExtTable) -> Self {
        let mut ranked: Vec<ExtId> = (0..exts.len() as u32).map(ExtId).collect();
        ranked.sort_by_key(|e| Reverse(exts.bytes[e.0 as usize]));
        ranked.truncate(DISTINCT);
        ranked.retain(|e| exts.bytes[e.0 as usize] > 0);
        let mut colors = vec![OTHER; exts.len()];
        for (rank, ext) in ranked.iter().enumerate() {
            colors[ext.0 as usize] = HUES[rank];
        }
        Palette { colors, ranked }
    }

    pub fn ext(&self, ext: ExtId) -> Rgb {
        self.colors[ext.0 as usize]
    }

    pub fn node(&self, node: &Node) -> Rgb {
        match node.kind {
            Kind::File { ext } => self.ext(ext),
            Kind::Dir { .. } => FOLDER,
            Kind::Other => OTHER,
        }
    }
}
