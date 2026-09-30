use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{
    self, Color32, ColorImage, Id, Label, Modal, Pos2, Rect as ERect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2,
};

use crate::color::{OTHER, Palette, Rgb};
use crate::human_size;
use crate::layout::{self, Rect, Tile};
use crate::render::{self, Raster};
use crate::scan::{self, Progress, ScanMsg};
use crate::tree::{NodeId, Tree};

const MIN_TILE_PX: f32 = 2.0;

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
    selected: Option<NodeId>,
    hovered: Option<NodeId>,
    confirm_trash: Option<NodeId>,
    notice: Option<String>,
    palette: Palette,
    /// Bumped whenever the tree changes, so the cached layout is rebuilt.
    revision: u64,
    layout: Option<LayoutCache>,
}

struct LayoutCache {
    key: (NodeId, [usize; 2], u64),
    tiles: Vec<Tile>,
    raster: Raster,
    texture: TextureHandle,
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
                    model.navigation(ui);
                }
            });
        });

        if let Phase::Ready(model) = &mut self.phase {
            egui::Panel::bottom("status").show(ui, |ui| model.status(ui));
            egui::Panel::right("legend")
                .resizable(false)
                .show(ui, |ui| model.legend(ui));
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
            selected: None,
            hovered: None,
            confirm_trash: None,
            notice: None,
            revision: 0,
            layout: None,
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
                Some(id) => {
                    ui.label(RichText::new(human_size(self.tree.node(id).size)).strong());
                    ui.add(Label::new(self.tree.path(id).display().to_string()).truncate());
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
        let key = (self.root, size_px, self.revision);
        if self.layout.as_ref().is_none_or(|l| l.key != key) {
            self.layout = Some(self.build_layout(ui.ctx(), key));
        }
        let cache = self.layout.as_ref().unwrap();

        let painter = ui.painter_at(area);
        painter.image(
            cache.texture.id(),
            area,
            ERect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        let hovered_tile = response.hover_pos().and_then(|pos| {
            let px = (pos - area.min) * ppp;
            cache.raster.tile_at(px.x as usize, px.y as usize)
        });
        // The view root shows through only where its children were too small to
        // draw; it is not a selectable item.
        self.hovered = hovered_tile
            .map(|i| cache.tiles[i].id)
            .filter(|&id| id != self.root);

        let to_screen = |r: Rect| {
            ERect::from_min_size(
                area.min + Vec2::new(r.x, r.y) / ppp,
                Vec2::new(r.w, r.h) / ppp,
            )
        };
        if let Some(id) = self.selected
            && let Some(tile) = cache.tiles.iter().find(|t| t.id == id)
        {
            painter.rect_stroke(
                to_screen(tile.rect),
                0.0,
                Stroke::new(2.0, Color32::YELLOW),
                StrokeKind::Inside,
            );
        }
        if let Some(id) = self.hovered
            && let Some(tile) = hovered_tile.map(|i| &cache.tiles[i])
        {
            debug_assert_eq!(tile.id, id);
            painter.rect_stroke(
                to_screen(tile.rect),
                0.0,
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Inside,
            );
        }

        if response.clicked() || response.secondary_clicked() {
            self.selected = self.hovered;
        }
        if response.double_clicked()
            && let Some(hit) = self.hovered
            && let Some(child) = self.tree.child_toward(self.root, hit)
            && self.tree.is_dir(child)
        {
            self.root = child;
        }

        response.context_menu(|ui| {
            let Some(id) = self.selected else {
                ui.close();
                return;
            };
            ui.label(RichText::new(&*self.tree.node(id).name).strong());
            ui.separator();
            if ui.button("Reveal in Finder").clicked() {
                if let Err(e) = Command::new("open")
                    .arg("-R")
                    .arg(self.tree.path(id))
                    .spawn()
                {
                    self.notice = Some(format!("Could not open Finder: {e}"));
                }
                ui.close();
            }
            if ui.button("Move to Trash…").clicked() {
                self.confirm_trash = Some(id);
                ui.close();
            }
        });
    }

    fn build_layout(&self, ctx: &egui::Context, key: (NodeId, [usize; 2], u64)) -> LayoutCache {
        let (root, [w, h], _) = key;
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        };
        let tiles = layout::layout(&self.tree, root, rect, MIN_TILE_PX);
        let mut raster =
            render::rasterize(&tiles, w, h, |t| self.palette.node(self.tree.node(t.id)));
        let image = ColorImage::from_rgba_unmultiplied([w, h], &std::mem::take(&mut raster.rgba));
        let texture = ctx.load_texture("treemap", image, TextureOptions::NEAREST);
        LayoutCache {
            key,
            tiles,
            raster,
            texture,
        }
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
                if self
                    .selected
                    .is_some_and(|s| self.tree.is_within(s, target))
                {
                    self.selected = None;
                }
                self.notice = None;
            }
            Err(e) => self.notice = Some(format!("Could not move to Trash: {e}")),
        }
    }
}

fn legend_row(ui: &mut egui::Ui, color: Rgb, name: &str, bytes: u64) {
    let (swatch, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter()
        .rect_filled(swatch, 2.0, Color32::from_rgb(color[0], color[1], color[2]));
    ui.label(name);
    ui.label(human_size(bytes));
    ui.end_row();
}
