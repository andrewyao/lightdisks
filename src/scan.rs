use std::cmp::Reverse;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io;
use std::ops::ControlFlow;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

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

/// One directory's counted entries, already assigned the raw indices
/// `first..first + entries.len()`.
struct Batch {
    dir: PathBuf,
    parent: u32,
    first: u32,
    entries: Vec<bulk::Entry>,
    errors: u64,
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
    let pool = rayon::ThreadPoolBuilder::new()
        .build()
        .map_err(|e| ScanError::Root(io::Error::other(e)))?;
    let (tx, rx) = mpsc::channel();
    let walker = Walker {
        dev: root_meta.dev(),
        dirs: Mutex::new(HashSet::from([(root_meta.dev(), root_meta.ino())])),
        next: AtomicU32::new(1),
        tx,
        cancelled: AtomicBool::new(false),
        containers: Mutex::new(()),
    };
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

    thread::scope(|s| {
        s.spawn(|| {
            // The walker, and with it the last sender, drops once every
            // directory is read, which ends the loop below.
            let walker = walker;
            pool.scope(|scope| walker.walk(scope, root.to_path_buf(), 0));
        });
        // Returning early drops `rx`, so the walker's next send fails and it stops.
        for batch in rx {
            builder.add(batch)?;
        }
        Ok(())
    })?;

    let errors = builder.progress.errors;
    Ok(build(
        builder.raws,
        builder.exts,
        root.to_path_buf(),
        errors,
    ))
}

struct Walker {
    dev: u64,
    /// Directories already queued. macOS firmlinks expose the Data volume at
    /// both `/Users` and `/System/Volumes/Data/Users`.
    dirs: Mutex<HashSet<(u64, u64)>>,
    next: AtomicU32,
    tx: Sender<Batch>,
    cancelled: AtomicBool,
    /// Held while reading inside another app's sandbox container. Each such
    /// open waits on sandboxd for approval, and one sometimes stalls for
    /// exactly 5s (macOS 26). Reading them one at a time makes that rare:
    /// 1 in 75 ~/Library scans, against 14 in 15 with no limit.
    containers: Mutex<()>,
}

impl Walker {
    /// Reads `dir`, whose raw index is `index`, reserves indices for the
    /// entries it counts, and queues its subdirectories. A child's index is
    /// reserved after its parent's, so parent < child for `build`.
    fn walk<'s>(&'s self, scope: &rayon::Scope<'s>, dir: PathBuf, index: u32) {
        if self.cancelled.load(Relaxed) {
            return;
        }
        let mut entries = Vec::new();
        let errors = self.read(&dir, index == 0, &mut entries);
        for e in entries.iter_mut().filter(|e| e.redirects) {
            // Rare, so one lstat each finds the identity of what it reaches.
            let path = dir.join(OsStr::from_bytes(&e.name));
            if let Ok(meta) = fs::symlink_metadata(path) {
                (e.dev, e.ino) = (meta.dev(), meta.ino());
            }
        }
        entries.retain(|e| self.counts(e));
        let first = self.next.fetch_add(entries.len() as u32, Relaxed);
        let subdirs: Vec<(PathBuf, u32)> = entries
            .iter()
            .zip(first..)
            .filter(|(e, _)| e.kind == bulk::EntryKind::Dir)
            .map(|(e, i)| (dir.join(OsStr::from_bytes(&e.name)), i))
            .collect();
        let batch = Batch {
            dir,
            parent: index,
            first,
            entries,
            errors,
        };
        if self.tx.send(batch).is_err() {
            self.cancelled.store(true, Relaxed);
            return;
        }
        for (path, i) in subdirs {
            scope.spawn(move |scope| self.walk(scope, path, i));
        }
    }

    /// Appends `dir`'s entries and returns how many things could not be read.
    fn read(&self, dir: &Path, is_root: bool, entries: &mut Vec<bulk::Entry>) -> u64 {
        let _one_at_a_time = in_container(dir).then(|| self.containers.lock().unwrap());
        // The root may be a symlink the user named; everything below is not.
        let opened = if is_root {
            File::open(dir)
        } else {
            bulk::open_dir(dir)
        };
        opened
            .and_then(|file| bulk::read_dir(&file, entries))
            .unwrap_or(1)
    }

    /// Whether an entry is on the scanned device and not a directory already
    /// reached by another path.
    fn counts(&self, e: &bulk::Entry) -> bool {
        e.dev == self.dev
            && (e.kind != bulk::EntryKind::Dir || self.dirs.lock().unwrap().insert((e.dev, e.ino)))
    }
}

const SANDBOX_PARENTS: [&str; 2] = ["Containers", "Group Containers"];

fn in_container(dir: &Path) -> bool {
    dir.parent()
        .is_some_and(|p| p.iter().any(|c| SANDBOX_PARENTS.iter().any(|s| c == *s)))
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
    fn add(&mut self, batch: Batch) -> Result<(), ScanError> {
        let first = batch.first as usize;
        let end = first + batch.entries.len();
        if self.raws.len() < end {
            self.raws.resize_with(end, || Raw {
                name: "".into(),
                parent: 0,
                size: 0,
                kind: RawKind::Other,
            });
        }
        for (i, e) in (first..end).zip(batch.entries) {
            let size = match e.kind {
                bulk::EntryKind::Dir => 0,
                _ if e.nlink > 1 && !self.linked.insert((e.dev, e.ino)) => 0,
                _ => e.alloc,
            };
            let name = String::from_utf8(e.name)
                .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned());
            let kind = match e.kind {
                bulk::EntryKind::Dir => RawKind::Dir,
                bulk::EntryKind::File => {
                    let ext = self.exts.intern(&extension_of(&name));
                    self.exts.bytes[ext.0 as usize] += size;
                    RawKind::File(ext)
                }
                bulk::EntryKind::Other => RawKind::Other,
            };
            self.raws[i] = Raw {
                name: name.into_boxed_str(),
                parent: batch.parent,
                size,
                kind,
            };
            self.progress.bytes += size;
        }
        self.progress.entries += (end - first) as u64;
        self.progress.errors += batch.errors;

        if self.last_report.elapsed() >= PROGRESS_INTERVAL {
            self.last_report = Instant::now();
            self.progress.current = batch.dir;
            if (self.on_progress)(&self.progress).is_break() {
                return Err(ScanError::Cancelled);
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

/// The only code that touches getattrlistbulk(2). Its packed reply is parsed
/// into owned `Entry` values here, with every read bounds-checked.
mod bulk {
    use std::cell::RefCell;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    use libc::{
        ATTR_BIT_MAP_COUNT, ATTR_CMN_DEVID, ATTR_CMN_FILEID, ATTR_CMN_FLAGS, ATTR_CMN_NAME,
        ATTR_CMN_OBJTYPE, ATTR_CMN_RETURNED_ATTRS, ATTR_DIR_MOUNTSTATUS, ATTR_FILE_ALLOCSIZE,
        ATTR_FILE_LINKCOUNT, DIR_MNTSTATUS_MNTPOINT, FSOPT_PACK_INVAL_ATTRS,
    };

    /// <sys/attr.h>; libc does not export it.
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    /// <sys/stat.h>; libc does not export it.
    const SF_FIRMLINK: u32 = 0x0080_0000;
    /// `enum vtype` in <sys/vnode.h>.
    const VREG: u32 = 1;
    const VDIR: u32 = 2;

    const COMMON: u32 = ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_NAME
        | ATTR_CMN_ERROR
        | ATTR_CMN_DEVID
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_FLAGS
        | ATTR_CMN_FILEID;
    const REQUIRED: u32 = ATTR_CMN_NAME | ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID;
    const FILE: u32 = ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE;
    const BUF_LEN: usize = 256 * 1024;

    thread_local! {
        static BUF: RefCell<Vec<u8>> = RefCell::new(vec![0; BUF_LEN]);
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum EntryKind {
        Dir,
        File,
        Other,
    }

    #[derive(Debug)]
    pub struct Entry {
        pub name: Vec<u8>,
        pub kind: EntryKind,
        pub dev: u64,
        pub ino: u64,
        pub nlink: u64,
        /// Allocated bytes; zero for directories.
        pub alloc: u64,
        /// A firmlink or a mount point. `dev` and `ino` describe the entry
        /// itself, not the directory that opening it reaches.
        pub redirects: bool,
    }

    pub fn open_dir(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
    }

    /// Appends the entries of the open directory `dir` to `out` and returns
    /// how many entries the kernel could not describe.
    pub fn read_dir(dir: &File, out: &mut Vec<Entry>) -> io::Result<u64> {
        BUF.with_borrow_mut(|buf| {
            let mut unreadable = 0;
            loop {
                let count = fill(dir, buf)?;
                if count == 0 {
                    return Ok(unreadable);
                }
                let mut rest = &buf[..];
                for _ in 0..count {
                    let len = Fields::new(rest).u32()? as usize;
                    if len < 4 || len > rest.len() {
                        return Err(malformed());
                    }
                    let (record, tail) = rest.split_at(len);
                    rest = tail;
                    match parse(record)? {
                        Some(entry) => out.push(entry),
                        None => unreadable += 1,
                    }
                }
            }
        })
    }

    fn fill(dir: &File, buf: &mut [u8]) -> io::Result<usize> {
        let mut list = libc::attrlist {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: COMMON,
            volattr: 0,
            dirattr: ATTR_DIR_MOUNTSTATUS,
            fileattr: FILE,
            forkattr: 0,
        };
        // SAFETY: `list` and `buf` outlive the call, and the kernel writes at
        // most `buf.len()` bytes into `buf`.
        let n = unsafe {
            libc::getattrlistbulk(
                dir.as_raw_fd(),
                (&raw mut list).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                FSOPT_PACK_INVAL_ATTRS as u64,
            )
        };
        usize::try_from(n).map_err(|_| io::Error::last_os_error())
    }

    /// One record: its length, the returned attribute_set_t, the error slot,
    /// the common attributes in bit order, then the directory group for a
    /// directory or the file group for anything else. This order was observed
    /// on macOS 26: the error slot sits before the name, and
    /// FSOPT_PACK_INVAL_ATTRS keeps it present with no error but packs only
    /// the group matching the object's type.
    /// `None` is an entry the kernel reported an error for.
    fn parse(record: &[u8]) -> io::Result<Option<Entry>> {
        let mut f = Fields::new(record);
        f.u32()?;
        let returned_common = f.u32()?;
        let _returned_vol = f.u32()?;
        let returned_dir = f.u32()?;
        let returned_file = f.u32()?;
        let _returned_fork = f.u32()?;
        let error = f.u32()?;
        let name_ref = f.at;
        let name_offset = f.i32()?;
        let name_len = f.u32()?;
        let dev = f.i32()?;
        let objtype = f.u32()?;
        let flags = f.u32()?;
        let ino = f.u64()?;

        if (returned_common & ATTR_CMN_ERROR != 0 && error != 0)
            || returned_common & REQUIRED != REQUIRED
        {
            return Ok(None);
        }
        let name = (name_ref as i64)
            .checked_add(name_offset.into())
            .and_then(|start| usize::try_from(start).ok())
            .and_then(|start| record.get(start..start.checked_add(name_len as usize)?))
            .ok_or_else(malformed)?;
        let name = name.split(|&b| b == 0).next().unwrap_or_default();
        let kind = match objtype {
            VDIR => EntryKind::Dir,
            VREG => EntryKind::File,
            _ => EntryKind::Other,
        };
        let (mut nlink, mut alloc, mut redirects) = (1, 0, false);
        if kind == EntryKind::Dir {
            let status = f.u32()?;
            redirects = (returned_dir & ATTR_DIR_MOUNTSTATUS != 0
                && status & DIR_MNTSTATUS_MNTPOINT != 0)
                || (returned_common & ATTR_CMN_FLAGS != 0 && flags & SF_FIRMLINK != 0);
        }
        if kind != EntryKind::Dir {
            let (count, size) = (f.u32()?, f.i64()?);
            if returned_file & ATTR_FILE_LINKCOUNT != 0 {
                nlink = count.into();
            }
            if returned_file & ATTR_FILE_ALLOCSIZE != 0 {
                alloc = size.max(0) as u64;
            }
        }
        Ok(Some(Entry {
            name: name.to_vec(),
            kind,
            dev: dev as u64,
            ino,
            nlink,
            alloc,
            redirects,
        }))
    }

    fn malformed() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed getattrlistbulk record",
        )
    }

    struct Fields<'a> {
        record: &'a [u8],
        at: usize,
    }

    impl<'a> Fields<'a> {
        fn new(record: &'a [u8]) -> Self {
            Fields { record, at: 0 }
        }

        fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
            let bytes = self
                .record
                .get(self.at..self.at + N)
                .ok_or_else(malformed)?;
            self.at += N;
            Ok(bytes.try_into().unwrap())
        }

        fn u32(&mut self) -> io::Result<u32> {
            self.take().map(u32::from_ne_bytes)
        }

        fn i32(&mut self) -> io::Result<i32> {
            self.take().map(i32::from_ne_bytes)
        }

        fn u64(&mut self) -> io::Result<u64> {
            self.take().map(u64::from_ne_bytes)
        }

        fn i64(&mut self) -> io::Result<i64> {
            self.take().map(i64::from_ne_bytes)
        }
    }
}
