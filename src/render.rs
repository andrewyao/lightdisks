use rayon::prelude::*;

use crate::color::Rgb;
use crate::layout::Tile;

const NO_TILE: u32 = u32::MAX;
const AMBIENT: f32 = 0.13;
const DIFFUSE: f32 = 0.87;
const BACKGROUND: [u8; 4] = [24, 24, 27, 255];

/// A shaded treemap image plus, for every pixel, the index of the deepest tile
/// covering it.
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    hit: Vec<u32>,
}

impl Raster {
    pub fn tile_at(&self, x: usize, y: usize) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = self.hit[y * self.width + x];
        (index != NO_TILE).then_some(index as usize)
    }
}

/// Paints `tiles` (pre-order, in pixel coordinates) with Van Wijk cushion
/// shading, lit from the top left.
pub fn rasterize(
    tiles: &[Tile],
    width: usize,
    height: usize,
    color: impl Fn(&Tile) -> Rgb,
) -> Raster {
    let mut hit = vec![NO_TILE; width * height];
    for (i, tile) in tiles.iter().enumerate() {
        let r = tile.rect;
        let x0 = (r.x.round().max(0.0) as usize).min(width);
        let x1 = ((r.x + r.w).round().max(0.0) as usize).min(width);
        let y0 = (r.y.round().max(0.0) as usize).min(height);
        let y1 = ((r.y + r.h).round().max(0.0) as usize).min(height);
        for y in y0..y1 {
            hit[y * width + x0..y * width + x1].fill(i as u32);
        }
    }

    let colors: Vec<Rgb> = tiles.iter().map(color).collect();
    let light = normalize([-1.0, -1.0, 10.0]);
    let mut rgba = vec![0u8; width * height * 4];
    rgba.par_chunks_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let py = y as f32 + 0.5;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let index = hit[y * width + x];
                if index == NO_TILE {
                    px.copy_from_slice(&BACKGROUND);
                    continue;
                }
                let c = tiles[index as usize].cushion;
                let px_x = x as f32 + 0.5;
                let nx = -(2.0 * c[0] * px_x + c[1]);
                let ny = -(2.0 * c[2] * py + c[3]);
                let cos =
                    (nx * light[0] + ny * light[1] + light[2]) / (nx * nx + ny * ny + 1.0).sqrt();
                let intensity = AMBIENT + DIFFUSE * cos.max(0.0);
                let base = colors[index as usize];
                for k in 0..3 {
                    px[k] = (base[k] as f32 * intensity).min(255.0) as u8;
                }
                px[3] = 255;
            }
        });

    Raster {
        width,
        height,
        rgba,
        hit,
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}
