use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Id, Label, Modal, Painter, Pos2, Rect as ERect,
    RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2,
};

use crate::color::{self, OTHER, Palette, Rgb};
use crate::hidden::Hidden;
use crate::human_size;
use crate::layout::{self, FolderTile, Item, Rect, Tile};
use crate::render::{self, Raster};
use crate::scan::{self, Progress, ScanMsg};
use crate::tree::{NodeId, Tree};

const MIN_TILE_PX: f32 = 2.0;
const MIN_FOLDER_PT: f32 = 3.0;
const MAX_DEPTH: u8 = 4;
const DEFAULT_DEPTH: u8 = 2;
const LABEL_FONT: FontId = FontId::proportional(12.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Folders { depth: u8 },
    FileTypes,
}

pub enum Phase {
    Picking,
    Scanning {
        root: PathBuf,
        rx: Receiver<ScanMsg>,
        progress: Progress,
    },
    Ready(Box<Model>),
    Failed(String),
}

pub struct Model {
    tree: Tree,
    /// The directory filling the treemap.
    root: NodeId,
    view: View,
    selected: Option<Item>,
    hovered: Option<Item>,
    confirm_trash: Option<NodeId>,
    notice: Option<String>,
    palette: Palette,
    hidden: Hidden,
    /// Bumped whenever the tree or `hidden` changes, so the cached layout is rebuilt.
    revision: u64,
    layout: Option<LayoutCache>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LayoutKey {
    root: NodeId,
    size_px: [usize; 2],
    revision: u64,
    view: View,
}

struct LayoutCache {
    key: LayoutKey,
    drawn: Drawn,
}

/// Folder tiles are laid out in points and painted as shapes each frame; file
/// tiles are laid out in pixels and shaded once into a texture.
enum Drawn {
    Folders {
        tiles: Vec<FolderTile>,
        fills: Vec<Rgb>,
    },
    FileTypes {
        tiles: Vec<Tile>,
        raster: Raster,
        texture: TextureHandle,
    },
}

pub struct App {
    phase: Phase,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let phase = match path.or_else(pick_folder) {
            Some(path) => start_scan(&cc.egui_ctx, path),
            None => Phase::Picking,
        };
        App { phase }
    }

    fn poll_scan(&mut self) {
        let Phase::Scanning { rx, progress, .. } = &mut self.phase else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(ScanMsg::Progress(p)) => *progress = p,
                Ok(ScanMsg::Done(tree)) => {
                    self.phase = Phase::Ready(Box::new(Model::new(tree)));
                    return;
                }
                Ok(ScanMsg::Failed(e)) => {
                    self.phase = Phase::Failed(e);
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.phase = Phase::Failed("the scan stopped unexpectedly".into());
                    return;
                }
            }
        }
    }
}

fn pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a folder to scan")
        .pick_folder()
}

fn start_scan(ctx: &egui::Context, root: PathBuf) -> Phase {
    let ctx = ctx.clone();
    let rx = scan::spawn(root.clone(), move || ctx.request_repaint());
    Phase::Scanning {
        root,
        rx,
        progress: Progress::default(),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_scan();
        let mut open = false;

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                open = ui.button("Open…").clicked();
                if let Phase::Ready(model) = &mut self.phase {
                    ui.separator();
                    model.view_controls(ui);
                    ui.separator();
                    model.navigation(ui);
                }
            });
        });

        if let Phase::Ready(model) = &mut self.phase {
            egui::Panel::bottom("status").show(ui, |ui| model.status(ui));
            egui::Panel::right("legend")
                .resizable(false)
                .show(ui, |ui| match model.view {
                    View::Folders { .. } => model.folder_list(ui),
                    View::FileTypes => model.legend(ui),
                });
        }

        egui::CentralPanel::default().show(ui, |ui| match &mut self.phase {
            Phase::Picking => {
                ui.centered_and_justified(|ui| {
                    open |= ui.button("Choose a folder to scan…").clicked();
                });
            }
            Phase::Scanning { root, progress, .. } => {
                if scanning(ui, root, progress) {
                    self.phase = Phase::Picking;
                }
            }
            Phase::Ready(model) => model.treemap(ui),
            Phase::Failed(error) => {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new(format!("Scan failed: {error}")).color(Color32::LIGHT_RED),
                    );
                });
            }
        });

        if let Phase::Ready(model) = &mut self.phase {
            model.trash_dialog(ui.ctx());
        }

        if open && let Some(path) = pick_folder() {
            self.phase = start_scan(ui.ctx(), path);
        }
    }
}

/// Returns true when the user cancels.
fn scanning(ui: &mut egui::Ui, root: &std::path::Path, progress: &Progress) -> bool {
    let mut cancel = false;
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.spinner();
        ui.heading(format!("Scanning {}", root.display()));
        ui.label(format!(
            "{} items, {}{}",
            progress.entries,
            human_size(progress.bytes),
            match progress.errors {
                0 => String::new(),
                n => format!(", {n} unreadable"),
            }
        ));
        ui.add(Label::new(RichText::new(progress.current.display().to_string()).weak()).truncate());
        cancel = ui.button("Cancel").clicked();
    });
    cancel
}

impl Model {
    fn new(tree: Tree) -> Self {
        Model {
            palette: Palette::new(&tree.exts),
            tree,
            root: Tree::ROOT,
            view: View::Folders {
                depth: DEFAULT_DEPTH,
            },
            selected: None,
            hovered: None,
            confirm_trash: None,
            notice: None,
            hidden: Hidden::default(),
            revision: 0,
            layout: None,
        }
    }

    fn view_controls(&mut self, ui: &mut egui::Ui) {
        let folders = matches!(self.view, View::Folders { .. });
        if ui.selectable_label(folders, "Folders").clicked() && !folders {
            self.view = View::Folders {
                depth: DEFAULT_DEPTH,
            };
        }
        if ui.selectable_label(!folders, "File types").clicked() {
            self.view = View::FileTypes;
        }
        if let View::Folders { depth } = &mut self.view {
            ui.add(egui::Slider::new(depth, 1..=MAX_DEPTH).text("depth"));
        }
    }

    fn item_name(&self, item: Item) -> &str {
        match item {
            Item::Node(id) => &self.tree.node(id).name,
            Item::Files { .. } => "(files)",
        }
    }

    fn navigation(&mut self, ui: &mut egui::Ui) {
        let parent = self.tree.node(self.root).parent;
        if ui
            .add_enabled(parent.is_some(), egui::Button::new("Up"))
            .clicked()
        {
            self.root = parent.unwrap_or(self.root);
        }
        ui.separator();
        let mut crumbs: Vec<NodeId> = self.tree.ancestors(self.root).collect();
        crumbs.reverse();
        for (i, &crumb) in crumbs.iter().enumerate() {
            if i > 0 {
                ui.label("›");
            }
            if ui
                .selectable_label(crumb == self.root, &*self.tree.node(crumb).name)
                .clicked()
            {
                self.root = crumb;
            }
        }
    }

    fn status(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(notice) = &self.notice {
                ui.label(RichText::new(notice).color(Color32::LIGHT_RED));
                ui.separator();
            }
            if self.tree.errors > 0 {
                ui.label(format!("{} unreadable", self.tree.errors));
                ui.separator();
            }
            match self.hovered.or(self.selected) {
                Some(item) => {
                    let bytes = item.bytes(&self.tree, &self.hidden);
                    ui.label(RichText::new(human_size(bytes)).strong());
                    let root_bytes = self.tree.node(self.root).size;
                    if root_bytes > 0 && self.tree.is_within(item.node(), self.root) {
                        ui.label(format!("{:.1}%", 100.0 * bytes as f64 / root_bytes as f64));
                    }
                    let mut path = self.tree.path(item.node()).display().to_string();
                    if let Item::Files { .. } = item {
                        path.push_str("  (files)");
                    }
                    ui.add(Label::new(path).truncate());
                }
                None => {
                    ui.label(format!(
                        "{} total",
                        human_size(self.tree.node(self.root).size)
                    ));
                }
            }
        });
    }

    fn legend(&self, ui: &mut egui::Ui) {
        ui.heading("File types");
        ui.add_space(4.0);
        let exts = &self.tree.exts;
        let mut shown = 0;
        egui::Grid::new("legend")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for &ext in &self.palette.ranked {
                    let bytes = exts.bytes[ext.0 as usize];
                    shown += bytes;
                    let name = match exts.name(ext) {
                        "" => "(no extension)".to_string(),
                        name => format!(".{name}"),
                    };
                    legend_row(ui, self.palette.ext(ext), &name, bytes);
                }
                let total: u64 = exts.bytes.iter().sum();
                legend_row(ui, OTHER, "Other", total - shown);
            });
    }

    fn folder_list(&mut self, ui: &mut egui::Ui) {
        ui.heading(&*self.tree.node(self.root).name);
        ui.add_space(4.0);
        let hues = top_hues(&self.tree, &self.hidden, self.root);
        let root_bytes = self.tree.node(self.root).size.max(1);
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("folders")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    for (item, bytes) in layout::items(&self.tree, &self.hidden, self.root) {
                        let fill = match item {
                            Item::Node(id) => color::folder_fill(hues[&id], 1),
                            Item::Files { .. } => color::files_fill(None, 1),
                        };
                        swatch(ui, fill);
                        let name = self.item_name(item);
                        let short: String = if name.chars().count() > 28 {
                            name.chars().take(27).chain(['…']).collect()
                        } else {
                            name.into()
                        };
                        let row = ui
                            .selectable_label(self.selected == Some(item), short)
                            .on_hover_text(name);
                        if row.clicked() {
                            self.selected = Some(item);
                        }
                        ui.label(human_size(bytes));
                        ui.label(format!("{:.1}%", 100.0 * bytes as f64 / root_bytes as f64));
                        ui.end_row();
                    }
                });
        });
    }

    fn treemap(&mut self, ui: &mut egui::Ui) {
        let (area, response) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let ppp = ui.ctx().pixels_per_point();
        let size_px = [
            (area.width() * ppp).round() as usize,
            (area.height() * ppp).round() as usize,
        ];
        if size_px[0] == 0 || size_px[1] == 0 {
            return;
        }
        let key = LayoutKey {
            root: self.root,
            size_px,
            revision: self.revision,
            view: self.view,
        };
        if self.layout.as_ref().is_none_or(|l| l.key != key) {
            self.layout = Some(self.build_layout(ui.ctx(), key, area.size()));
        }
        let cache = self.layout.as_ref().unwrap();

        let painter = ui.painter_at(area);
        let pointer = response.hover_pos();
        let (hovered, selected_rect) = match &cache.drawn {
            Drawn::Folders { tiles, fills } => {
                let to_screen = |r: Rect| {
                    ERect::from_min_size(area.min + Vec2::new(r.x, r.y), Vec2::new(r.w, r.h))
                };
                for (tile, &fill) in tiles.iter().zip(fills) {
                    let name = self.item_name(tile.item);
                    paint_folder(
                        &painter,
                        tile,
                        to_screen,
                        fill,
                        name,
                        tile.item.bytes(&self.tree, &self.hidden),
                    );
                }
                let hovered = pointer.and_then(|p| {
                    tiles
                        .iter()
                        .rfind(|t| to_screen(t.rect).contains(p))
                        .map(|t| (t.item, to_screen(t.rect)))
                });
                let selected = self
                    .selected
                    .and_then(|s| tiles.iter().find(|t| t.item == s))
                    .map(|t| to_screen(t.rect));
                (hovered, selected)
            }
            Drawn::FileTypes {
                tiles,
                raster,
                texture,
            } => {
                painter.image(
                    texture.id(),
                    area,
                    ERect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                let to_screen = |r: Rect| {
                    ERect::from_min_size(
                        area.min + Vec2::new(r.x, r.y) / ppp,
                        Vec2::new(r.w, r.h) / ppp,
                    )
                };
                // The view root shows through only where its children were too
                // small to draw; it is not a selectable item.
                let hovered = pointer
                    .and_then(|pos| {
                        let px = (pos - area.min) * ppp;
                        raster.tile_at(px.x as usize, px.y as usize)
                    })
                    .map(|i| &tiles[i])
                    .filter(|t| t.id != self.root)
                    .map(|t| (Item::Node(t.id), to_screen(t.rect)));
                let selected = match self.selected {
                    Some(Item::Node(id)) => tiles.iter().find(|t| t.id == id),
                    _ => None,
                }
                .map(|t| to_screen(t.rect));
                (hovered, selected)
            }
        };
        self.hovered = hovered.map(|(item, _)| item);

        if let Some(rect) = selected_rect {
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(2.0, Color32::YELLOW),
                StrokeKind::Inside,
            );
        }
        if let Some((_, rect)) = hovered {
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Inside,
            );
        }

        if response.clicked() || response.secondary_clicked() {
            self.selected = self.hovered;
        }
        if response.double_clicked()
            && let Some(dir) = self.hovered.and_then(|item| self.zoom_target(item))
        {
            self.root = dir;
        }

        response.context_menu(|ui| {
            let Some(item) = self.selected else {
                ui.close();
                return;
            };
            ui.label(RichText::new(self.item_name(item)).strong());
            ui.separator();
            if ui.button("Reveal in Finder").clicked() {
                if let Err(e) = Command::new("open")
                    .arg("-R")
                    .arg(self.tree.path(item.node()))
                    .spawn()
                {
                    self.notice = Some(format!("Could not open Finder: {e}"));
                }
                ui.close();
            }
            let trash = ui.add_enabled(
                matches!(item, Item::Node(_)),
                egui::Button::new("Move to Trash…"),
            );
            if trash.clicked() {
                self.confirm_trash = Some(item.node());
                ui.close();
            }
        });
    }

    fn zoom_target(&self, item: Item) -> Option<NodeId> {
        let Item::Node(id) = item else {
            return None;
        };
        let target = match self.view {
            View::Folders { .. } => id,
            View::FileTypes => self.tree.child_toward(self.root, id)?,
        };
        self.tree.is_dir(target).then_some(target)
    }

    fn build_layout(&self, ctx: &egui::Context, key: LayoutKey, points: Vec2) -> LayoutCache {
        let drawn = match key.view {
            View::Folders { depth } => {
                let rect = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: points.x,
                    h: points.y,
                };
                let tiles = layout::folders(
                    &self.tree,
                    &self.hidden,
                    key.root,
                    rect,
                    depth,
                    MIN_FOLDER_PT,
                );
                let fills = folder_fills(&self.tree, &self.hidden, key.root, &tiles);
                Drawn::Folders { tiles, fills }
            }
            View::FileTypes => {
                let [w, h] = key.size_px;
                let rect = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: w as f32,
                    h: h as f32,
                };
                let tiles = layout::layout(&self.tree, &self.hidden, key.root, rect, MIN_TILE_PX);
                let mut raster =
                    render::rasterize(&tiles, w, h, |t| self.palette.node(self.tree.node(t.id)));
                let image =
                    ColorImage::from_rgba_unmultiplied([w, h], &std::mem::take(&mut raster.rgba));
                let texture = ctx.load_texture("treemap", image, TextureOptions::NEAREST);
                Drawn::FileTypes {
                    tiles,
                    raster,
                    texture,
                }
            }
        };
        LayoutCache { key, drawn }
    }

    fn trash_dialog(&mut self, ctx: &egui::Context) {
        let Some(target) = self.confirm_trash else {
            return;
        };
        let path = self.tree.path(target);
        let mut confirmed = false;
        let mut cancelled = false;
        let modal = Modal::new(Id::new("confirm-trash")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Move to Trash?");
            ui.add(Label::new(path.display().to_string()).wrap());
            ui.label(format!(
                "{} will be moved to the Trash.",
                human_size(self.tree.node(target).size)
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                confirmed = ui.button("Move to Trash").clicked();
                cancelled = ui.button("Cancel").clicked();
            });
        });
        if cancelled || modal.should_close() {
            self.confirm_trash = None;
        }
        if !confirmed {
            return;
        }
        self.confirm_trash = None;
        match trash::delete(&path) {
            Ok(()) => {
                self.tree.remove(target);
                self.revision += 1;
                self.selected = self.selected.and_then(|sel| match sel {
                    _ if self.tree.is_within(sel.node(), target) => None,
                    // Trashing a file shrinks its folder's (files) tile.
                    Item::Files { dir, .. } => layout::items(&self.tree, &self.hidden, dir)
                        .into_iter()
                        .find_map(|(item, _)| matches!(item, Item::Files { .. }).then_some(item)),
                    Item::Node(_) => Some(sel),
                });
                self.notice = None;
            }
            Err(e) => self.notice = Some(format!("Could not move to Trash: {e}")),
        }
    }
}

/// One hue per folder in the view root, by size rank.
fn top_hues(tree: &Tree, hidden: &Hidden, root: NodeId) -> HashMap<NodeId, Rgb> {
    layout::items(tree, hidden, root)
        .into_iter()
        .filter_map(|(item, _)| match item {
            Item::Node(id) => Some(id),
            Item::Files { .. } => None,
        })
        .enumerate()
        .map(|(rank, id)| (id, color::folder_hue(rank)))
        .collect()
}

/// Each tile takes the hue of its top-level ancestor, which in pre-order is
/// the latest depth-1 tile.
fn folder_fills(tree: &Tree, hidden: &Hidden, root: NodeId, tiles: &[FolderTile]) -> Vec<Rgb> {
    let hues = top_hues(tree, hidden, root);
    let mut hue = None;
    tiles
        .iter()
        .map(|t| {
            if t.depth == 1 {
                hue = match t.item {
                    Item::Node(id) => Some(hues[&id]),
                    Item::Files { .. } => None,
                };
            }
            match t.item {
                Item::Node(_) => color::folder_fill(hue.unwrap_or(OTHER), t.depth),
                Item::Files { .. } => color::files_fill(hue, t.depth),
            }
        })
        .collect()
}

fn paint_folder(
    painter: &Painter,
    tile: &FolderTile,
    to_screen: impl Fn(Rect) -> ERect,
    fill: Rgb,
    name: &str,
    bytes: u64,
) {
    let rect = to_screen(tile.rect);
    painter.rect_filled(rect, 0.0, rgb(fill));
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, Color32::from_black_alpha(160)),
        StrokeKind::Inside,
    );
    let ink = rgb(color::ink(fill));
    let size = human_size(bytes);
    if let Some(header) = tile.header {
        let header = to_screen(header).shrink2(Vec2::new(4.0, 0.0));
        painter
            .with_clip_rect(header.intersect(painter.clip_rect()))
            .text(
                header.left_center(),
                Align2::LEFT_CENTER,
                format!("{name}  {size}"),
                LABEL_FONT,
                ink,
            );
        return;
    }
    let room = rect.shrink(3.0);
    let name = painter.layout_no_wrap(name.to_owned(), LABEL_FONT, ink);
    if name.size().x > room.width() || name.size().y > room.height() {
        return;
    }
    let size = painter.layout_no_wrap(size, LABEL_FONT, ink);
    let both = size.size().x <= room.width() && name.size().y + size.size().y <= room.height();
    let lines = if both { vec![name, size] } else { vec![name] };
    let mut y = room.center().y - lines.iter().map(|g| g.size().y).sum::<f32>() / 2.0;
    for galley in lines {
        let pos = Pos2::new(room.center().x - galley.size().x / 2.0, y);
        y += galley.size().y;
        painter.galley(pos, galley, ink);
    }
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn swatch(ui: &mut egui::Ui, color: Rgb) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, rgb(color));
}

fn legend_row(ui: &mut egui::Ui, color: Rgb, name: &str, bytes: u64) {
    swatch(ui, color);
    ui.label(name);
    ui.label(human_size(bytes));
    ui.end_row();
}
