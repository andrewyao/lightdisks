use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use lightdisks::app::App;
use lightdisks::human_size;
use lightdisks::scan;
use lightdisks::tree::{Kind, Tree};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, path] if flag == "--dump" => dump(Path::new(path)),
        [flag, ..] if flag.starts_with('-') => {
            eprintln!("usage: lightdisks [<folder>] | lightdisks --dump <folder>");
            ExitCode::FAILURE
        }
        [path] => gui(Some(Path::new(path))),
        [] => gui(None),
        _ => {
            eprintln!("usage: lightdisks [<folder>] | lightdisks --dump <folder>");
            ExitCode::FAILURE
        }
    }
}

fn gui(path: Option<&Path>) -> ExitCode {
    let path = match path.map(std::path::absolute).transpose() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("lightdisks: {e}");
            return ExitCode::FAILURE;
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("lightdisks")
            .with_inner_size([1280.0, 820.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "lightdisks",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, path)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lightdisks: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dump(path: &Path) -> ExitCode {
    let started = Instant::now();
    let tree = match scan::scan(path, |_| ControlFlow::Continue(())) {
        Ok(tree) => tree,
        Err(e) => {
            eprintln!("lightdisks: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let root = tree.node(Tree::ROOT);
    println!(
        "{}  {} ({} bytes), {} entries, {} unreadable, {:.2}s",
        PathBuf::from(&*root.name).display(),
        human_size(root.size),
        root.size,
        tree.nodes.len() - 1,
        tree.errors,
        started.elapsed().as_secs_f64(),
    );
    for child in tree.children(Tree::ROOT).take(20) {
        let node = tree.node(child);
        let slash = if matches!(node.kind, Kind::Dir { .. }) {
            "/"
        } else {
            ""
        };
        println!("{:>12}  {}{slash}", human_size(node.size), node.name);
    }
    ExitCode::SUCCESS
}
