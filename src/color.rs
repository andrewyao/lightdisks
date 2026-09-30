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

/// The hue of the `rank`-th largest top-level folder in folder view.
pub fn folder_hue(rank: usize) -> Rgb {
    HUES[rank % DISTINCT]
}

/// A folder tile's fill: dark at the top level and lighter at each depth, so a
/// folder's header stands apart from the children drawn over it.
pub fn folder_fill(hue: Rgb, depth: u8) -> Rgb {
    shade(hue, -0.3 + 0.18 * (depth.max(1) - 1) as f32)
}

/// A `(files)` tile's fill: a muted grey of its folder's hue, or plain grey
/// when the files sit directly in the view root.
pub fn files_fill(hue: Option<Rgb>, depth: u8) -> Rgb {
    let Some(hue) = hue else {
        return shade(OTHER, -0.3);
    };
    let grey = luma(hue);
    let muted = hue.map(|c| (grey * 0.75 + c as f32 * 0.25) as u8);
    folder_fill(muted, depth)
}

/// Black or white, whichever reads better on `fill`.
pub fn ink(fill: Rgb) -> Rgb {
    if luma(fill) > 150.0 {
        [16, 16, 18]
    } else {
        [255, 255, 255]
    }
}

fn luma(c: Rgb) -> f32 {
    0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32
}

/// Mixes toward black for negative `amount` and toward white for positive.
fn shade(c: Rgb, amount: f32) -> Rgb {
    let target = if amount < 0.0 { 0.0 } else { 255.0 };
    let t = amount.abs().min(1.0);
    c.map(|v| (v as f32 + (target - v as f32) * t).round() as u8)
}
