use std::cmp::Reverse;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io;
use std::ops::ControlFlow;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use jwalk::{Parallelism, WalkDirGeneric};

use crate::tree::{ExtId, ExtTable, Kind, Node, NodeId, Tree, extension_of};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub entries: u64,
    pub bytes: u64,
    pub errors: u64,
    pub current: PathBuf,
}

pub enum ScanMsg {
    Progress(Progress),
    Done(Tree),
    Failed(String),
}

#[derive(Debug)]
pub enum ScanError {
    Root(io::Error),
    Cancelled,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Root(e) => write!(f, "{e}"),
            ScanError::Cancelled => write!(f, "scan cancelled"),
        }
    }
}

/// What a directory reader learned about one entry.
#[derive(Debug, Default)]
enum Stat {
    /// Not counted: on another device, or a directory already reached by another
    /// path (macOS firmlinks expose the Data volume at both `/Users` and
    /// `/System/Volumes/Data/Users`).
    #[default]
    Skip,
    Unreadable,
    Walk(Meta),
    /// Opening directories inside other apps' sandbox containers from several
    /// threads at once intermittently stalls for exactly 5s in macOS's access
    /// check, while serial opens never do, so these subtrees are read from the
    /// single consumer thread.
    WalkSerially(Meta),
}

#[derive(Debug, Clone, Copy)]
struct Meta {
    is_dir: bool,
    is_file: bool,
    bytes: u64,
    dev: u64,
    ino: u64,
    nlink: u64,
}

const SANDBOX_PARENTS: [&str; 2] = ["Containers", "Group Containers"];

enum RawKind {
    Dir,
    File(ExtId),
    Other,
}

struct Raw {
    name: Box<str>,
    parent: u32,
    size: u64,
    kind: RawKind,
}

/// Scans in a background thread. The receiver gets throttled progress, then
/// exactly one `Done` or `Failed`. Dropping the receiver cancels the scan.
pub fn spawn(root: PathBuf, notify: impl Fn() + Send + 'static) -> Receiver<ScanMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = scan(&root, |p| {
            if tx.send(ScanMsg::Progress(p.clone())).is_err() {
                return ControlFlow::Break(());
            }
            notify();
            ControlFlow::Continue(())
        });
        let msg = match result {
            Ok(tree) => ScanMsg::Done(tree),
            Err(ScanError::Cancelled) => return,
            Err(e) => ScanMsg::Failed(format!("{}: {e}", root.display())),
        };
        let _ = tx.send(msg);
        notify();
    });
    rx
}

/// Walks `root` without following symlinks or crossing devices, sizing each
/// entry by allocated blocks and counting hard-linked files once.
pub fn scan(
    root: &Path,
    mut on_progress: impl FnMut(&Progress) -> ControlFlow<()>,
) -> Result<Tree, ScanError> {
    let root_meta = fs::metadata(root).map_err(ScanError::Root)?;
    if !root_meta.is_dir() {
        return Err(ScanError::Root(io::Error::new(
            io::ErrorKind::NotADirectory,
            "not a directory",
        )));
    }
    let seen = Arc::new(Seen {
        dev: root_meta.dev(),
        dirs: Mutex::new(HashSet::from([(root_meta.dev(), root_meta.ino())])),
    });

    let reader_seen = Arc::clone(&seen);
    let walk = WalkDirGeneric::<((), Stat)>::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(Parallelism::RayonNewPool(0))
        .process_read_dir(move |depth, dir, _, children| {
            // jwalk passes the root entry itself through here with no depth.
            if depth.is_none() {
                return;
            }
            let sandboxed = dir
                .file_name()
                .is_some_and(|n| SANDBOX_PARENTS.iter().any(|s| n == *s));
            for entry in children.iter_mut().flatten() {
                entry.client_state = match fs::symlink_metadata(dir.join(&entry.file_name)) {
                    Err(_) => Stat::Unreadable,
                    Ok(meta) => match reader_seen.classify(&meta) {
                        None => Stat::Skip,
                        Some(meta) if meta.is_dir && sandboxed => Stat::WalkSerially(meta),
                        Some(meta) => Stat::Walk(meta),
                    },
                };
                if !matches!(entry.client_state, Stat::Walk(_)) {
                    entry.read_children = None;
                }
            }
        });

    let mut builder = Builder {
        raws: vec![Raw {
            name: root.to_string_lossy().into(),
            parent: 0,
            size: 0,
            kind: RawKind::Dir,
        }],
        exts: ExtTable::default(),
        linked: HashSet::new(),
        progress: Progress::default(),
        last_report: Instant::now(),
        on_progress: &mut on_progress,
    };
    // stack[d] is the raw index of the directory currently open at depth d.
    let mut stack: Vec<u32> = vec![0];

    for item in walk {
        let Ok(entry) = item else {
            builder.progress.errors += 1;
            continue;
        };
        if entry.depth == 0 {
            continue;
        }
        if entry
            .read_children
            .as_ref()
            .is_some_and(|rc| rc.error().is_some())
        {
            builder.progress.errors += 1;
        }
        stack.truncate(entry.depth);
        let parent = stack[entry.depth - 1];
        let name = entry.file_name.to_string_lossy();
        match entry.client_state {
            Stat::Skip => {}
            Stat::Unreadable => builder.progress.errors += 1,
            Stat::Walk(meta) => {
                let index = builder.add(parent, &name, meta, || entry.path())?;
                if meta.is_dir {
                    stack.push(index);
                }
            }
            Stat::WalkSerially(meta) => {
                let path = entry.path();
                let index = builder.add(parent, &name, meta, || path.clone())?;
                builder.walk_serially(&path, index, &seen)?;
            }
        }
    }

    let errors = builder.progress.errors;
    Ok(build(
        builder.raws,
        builder.exts,
        root.to_path_buf(),
        errors,
    ))
}

struct Seen {
    dev: u64,
    dirs: Mutex<HashSet<(u64, u64)>>,
}

impl Seen {
    /// The entry's accounting facts, or `None` when it must not be counted.
    fn classify(&self, meta: &fs::Metadata) -> Option<Meta> {
        let (dev, ino) = (meta.dev(), meta.ino());
        if dev != self.dev || (meta.is_dir() && !self.dirs.lock().unwrap().insert((dev, ino))) {
            return None;
        }
        Some(Meta {
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
            bytes: meta.blocks() * 512,
            dev,
            ino,
            nlink: meta.nlink(),
        })
    }
}

struct Builder<'a> {
    raws: Vec<Raw>,
    exts: ExtTable,
    linked: HashSet<(u64, u64)>,
    progress: Progress,
    last_report: Instant,
    on_progress: &'a mut dyn FnMut(&Progress) -> ControlFlow<()>,
}

impl Builder<'_> {
    fn add(
        &mut self,
        parent: u32,
        name: &str,
        meta: Meta,
        path: impl FnOnce() -> PathBuf,
    ) -> Result<u32, ScanError> {
        let size = if meta.is_dir || (meta.nlink > 1 && !self.linked.insert((meta.dev, meta.ino))) {
            0
        } else {
            meta.bytes
        };
        let kind = if meta.is_dir {
            RawKind::Dir
        } else if meta.is_file {
            let ext = self.exts.intern(&extension_of(name));
            self.exts.bytes[ext.0 as usize] += size;
            RawKind::File(ext)
        } else {
            RawKind::Other
        };
        let index = self.raws.len() as u32;
        self.raws.push(Raw {
            name: name.into(),
            parent,
            size,
            kind,
        });

        self.progress.entries += 1;
        self.progress.bytes += size;
        if self.progress.entries.is_multiple_of(1024)
            && self.last_report.elapsed() >= PROGRESS_INTERVAL
        {
            self.last_report = Instant::now();
            self.progress.current = path();
            if (self.on_progress)(&self.progress).is_break() {
                return Err(ScanError::Cancelled);
            }
        }
        Ok(index)
    }

    fn walk_serially(&mut self, dir: &Path, parent: u32, seen: &Seen) -> Result<(), ScanError> {
        let Ok(read_dir) = fs::read_dir(dir) else {
            self.progress.errors += 1;
            return Ok(());
        };
        for entry in read_dir {
            let Ok(entry) = entry else {
                self.progress.errors += 1;
                continue;
            };
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                self.progress.errors += 1;
                continue;
            };
            let Some(meta) = seen.classify(&meta) else {
                continue;
            };
            let name = entry.file_name();
            let index = self.add(parent, &name.to_string_lossy(), meta, || path.clone())?;
            if meta.is_dir {
                self.walk_serially(&path, index, seen)?;
            }
        }
        Ok(())
    }
}

/// Lays raw pre-order entries out breadth-first so each directory's children
/// are contiguous, sorted by size descending.
fn build(mut raws: Vec<Raw>, exts: ExtTable, root_path: PathBuf, errors: u64) -> Tree {
    let n = raws.len();
    // Pre-order guarantees parent < child, so one reverse pass sums every dir.
    for i in (1..n).rev() {
        let parent = raws[i].parent as usize;
        raws[parent].size += raws[i].size;
    }

    let mut start = vec![0u32; n + 1];
    for raw in &raws[1..] {
        start[raw.parent as usize + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut cursor = start.clone();
    let mut kids = vec![0u32; n.saturating_sub(1)];
    for (i, raw) in raws.iter().enumerate().skip(1) {
        let p = raw.parent as usize;
        kids[cursor[p] as usize] = i as u32;
        cursor[p] += 1;
    }

    let mut nodes: Vec<Node> = Vec::with_capacity(n);
    let mut raw_of: Vec<u32> = Vec::with_capacity(n);
    nodes.push(Node {
        name: std::mem::take(&mut raws[0].name),
        parent: None,
        size: raws[0].size,
        kind: Kind::Dir { children: 0..0 },
    });
    raw_of.push(0);

    let mut i = 0;
    while i < nodes.len() {
        if matches!(nodes[i].kind, Kind::Dir { .. }) {
            let r = raw_of[i] as usize;
            let group = &mut kids[start[r] as usize..start[r + 1] as usize];
            group.sort_unstable_by_key(|&k| Reverse(raws[k as usize].size));
            let first = nodes.len() as u32;
            for &k in group.iter() {
                let raw = &mut raws[k as usize];
                let kind = match raw.kind {
                    RawKind::Dir => Kind::Dir { children: 0..0 },
                    RawKind::File(ext) => Kind::File { ext },
                    RawKind::Other => Kind::Other,
                };
                nodes.push(Node {
                    name: std::mem::take(&mut raw.name),
                    parent: Some(NodeId(i as u32)),
                    size: raw.size,
                    kind,
                });
                raw_of.push(k);
            }
            nodes[i].kind = Kind::Dir {
                children: first..nodes.len() as u32,
            };
        }
        i += 1;
    }

    Tree {
        nodes,
        exts,
        root_path,
        errors,
    }
}
