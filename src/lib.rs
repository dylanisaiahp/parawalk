//! # parawalk
//!
//! Blazing-fast parallel directory walker with zero filtering baggage.
//!
//! Uses a crossbeam-deque work-stealing scheduler — the same pattern as
//! the `ignore` crate's parallel walker, without any gitignore, glob, or
//! hidden-file filtering overhead.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use parawalk::{walk, WalkConfig, Entry, EntryRef};
//! use std::sync::mpsc;
//!
//! let (tx, rx) = mpsc::channel();
//!
//! walk(
//!     "/usr".into(),
//!     WalkConfig::default(),
//!     None::<fn(&EntryRef<'_>) -> bool>,
//!     move || {
//!         let tx = tx.clone();
//!         move |entry: Entry| { let _ = tx.send(entry); }
//!     },
//! );
//!
//! let count = rx.into_iter().count();
//! println!("Found {} entries", count);
//! ```

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_deque::{Injector, Stealer, Worker};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Configuration for a parallel walk.
pub struct WalkConfig {
    /// Number of worker threads. Defaults to logical CPU count.
    pub threads: usize,

    /// Maximum traversal depth. `None` = unlimited.
    pub max_depth: Option<usize>,

    /// Follow symbolic links. Defaults to false.
    pub follow_links: bool,
}

impl Default for WalkConfig {
    fn default() -> Self {
        Self {
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            max_depth: None,
            follow_links: false,
        }
    }
}

/// A single entry produced during a walk.
pub struct Entry {
    /// Full path to the entry.
    pub path: PathBuf,

    /// What kind of entry this is.
    pub kind: EntryKind,

    /// Depth from the root. Root's children = 1.
    pub depth: usize,
}

/// The kind of a directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Cheap borrowed view of an entry — available before any PathBuf is allocated.
///
/// Use this in your pre-filter to decide whether to materialize the full path.
pub struct EntryRef<'a> {
    /// Just the filename component — zero allocation.
    pub name: &'a OsStr,

    /// Depth from root.
    pub depth: usize,

    /// Entry kind.
    pub kind: EntryKind,
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

struct DirJob {
    path: PathBuf,
    depth: usize,
}

/// Shared walk context passed to process_dir — avoids too-many-arguments.
struct WalkCtx<P> {
    worker: Worker<DirJob>,
    injector: Arc<Injector<DirJob>>,
    pending: Arc<AtomicUsize>,
    pre_filter: Option<Arc<P>>,
    max_depth: Option<usize>,
    follow_links: bool,
}

// ---------------------------------------------------------------------------
// walk()
// ---------------------------------------------------------------------------

/// Walk a directory tree in parallel, calling `visitor` for each entry.
///
/// Only entries that pass the optional `pre_filter` are materialized into
/// full [`Entry`] values and passed to `visitor`. Entries that fail the
/// pre-filter are dropped with zero allocation.
///
/// # Arguments
///
/// * `root` - The directory to walk.
/// * `config` - Walk configuration (threads, depth, symlinks).
/// * `pre_filter` - Optional cheap filter on [`EntryRef`] (filename + kind).
///   Return `true` to materialize and visit the entry, `false` to skip.
/// * `visitor_factory` - Called once per thread to produce a per-thread visitor.
///   Each thread gets its own visitor instance — no shared state, no locking.
///   This mirrors `ignore`'s `build_parallel().run(|| Box::new(...))` pattern.
pub fn walk<F, V, P>(root: PathBuf, config: WalkConfig, pre_filter: Option<P>, visitor_factory: F)
where
    F: Fn() -> V + Send + Sync + 'static,
    V: FnMut(Entry) + Send + 'static,
    P: Fn(&EntryRef<'_>) -> bool + Send + Sync + 'static,
{
    let injector = Arc::new(Injector::<DirJob>::new());
    let visitor_factory = Arc::new(visitor_factory);
    let pre_filter: Option<Arc<P>> = pre_filter.map(Arc::new);

    // Seed the root job
    injector.push(DirJob {
        path: root,
        depth: 0,
    });

    let n = config.threads.max(1);
    let max_depth = config.max_depth;
    let follow_links = config.follow_links;

    // Build workers and stealers
    let workers: Vec<Worker<DirJob>> = (0..n).map(|_| Worker::new_lifo()).collect();
    let stealers: Arc<Vec<Stealer<DirJob>>> =
        Arc::new(workers.iter().map(|w| w.stealer()).collect());

    // Pending job counter — counts jobs that exist but haven't completed yet.
    // Initialized to 1 for the root job. Incremented BEFORE pushing each child
    // job, decremented AFTER process_dir returns. When it hits zero, the walk
    // is truly done — no jobs exist anywhere, in any thread's local queue or
    // the global injector.
    let pending = Arc::new(AtomicUsize::new(1));

    std::thread::scope(|s| {
        for worker in workers {
            let injector = Arc::clone(&injector);
            let stealers = Arc::clone(&stealers);
            let pre_filter = pre_filter.clone();
            let pending = Arc::clone(&pending);
            let mut visitor = visitor_factory();

            s.spawn(move || {
                let ctx = WalkCtx {
                    worker,
                    injector,
                    pending,
                    pre_filter,
                    max_depth,
                    follow_links,
                };

                loop {
                    let job = ctx.worker.pop().or_else(|| {
                        stealers
                            .iter()
                            .find_map(|s| s.steal().success())
                            .or_else(|| ctx.injector.steal().success())
                    });

                    match job {
                        Some(job) => {
                            process_dir(job, &mut visitor, &ctx);
                            // This job is complete. If we were the last pending
                            // job, all threads will see pending == 0 and exit.
                            ctx.pending.fetch_sub(1, Ordering::Release);
                        }
                        None => {
                            if ctx.pending.load(Ordering::Acquire) == 0 {
                                break;
                            }
                            std::thread::yield_now();
                        }
                    }
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// process_dir
// ---------------------------------------------------------------------------

fn process_dir<V, P>(job: DirJob, visitor: &mut V, ctx: &WalkCtx<P>)
where
    V: FnMut(Entry),
    P: Fn(&EntryRef<'_>) -> bool + Send + Sync,
{
    let read = match fs::read_dir(&job.path) {
        Ok(r) => r,
        Err(_) => return,
    };

    for raw in read {
        let raw = match raw {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_type = match raw.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        let is_symlink = file_type.is_symlink();
        let is_dir = if is_symlink && ctx.follow_links {
            raw.path().is_dir()
        } else {
            file_type.is_dir()
        };

        let kind = if is_dir {
            EntryKind::Dir
        } else if is_symlink {
            EntryKind::Symlink
        } else if file_type.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };

        let depth = job.depth + 1;
        let name = raw.file_name();

        // Cheap pre-filter — runs on borrowed &OsStr, zero allocation
        let pass = ctx
            .pre_filter
            .as_ref()
            .map(|f| {
                f(&EntryRef {
                    name: &name,
                    depth,
                    kind: kind.clone(),
                })
            })
            .unwrap_or(true);

        if pass {
            let path = job.path.join(&name);
            visitor(Entry {
                path,
                kind: kind.clone(),
                depth,
            });
        }

        // Push subdirectories as new jobs regardless of filter
        if is_dir && ctx.max_depth.map(|d| depth < d).unwrap_or(true) {
            // Increment BEFORE pushing so pending is never zero while work exists
            ctx.pending.fetch_add(1, Ordering::Relaxed);
            ctx.worker.push(DirJob {
                path: job.path.join(&name),
                depth,
            });
        }
    }
}
