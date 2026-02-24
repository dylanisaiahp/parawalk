# parawalk — Documentation

## Overview

parawalk walks a directory tree in parallel using a work-stealing scheduler.
It is intentionally minimal — no gitignore parsing, no glob filtering, no
hidden-file rules. All filtering logic belongs in the caller.

The design is built around two key ideas:

1. **Per-thread visitors** — instead of sharing a single visitor across threads
   (which requires a `Mutex`), parawalk calls a factory closure once per thread.
   Each thread gets its own visitor instance with its own local state.

2. **Pre-filter before PathBuf** — an optional pre-filter runs on cheap borrowed
   data (`&OsStr` filename, depth, kind) before any `PathBuf` is materialized.
   Entries that don't pass the filter are dropped with zero allocation.

---

## API

### `walk()`

```rust
pub fn walk<F, V, P>(
    root: PathBuf,
    config: WalkConfig,
    pre_filter: Option<P>,
    visitor_factory: F,
)
where
    F: Fn() -> V + Send + Sync + 'static,
    V: FnMut(Entry) + Send + 'static,
    P: Fn(&EntryRef<'_>) -> bool + Send + Sync + 'static,
```

Walks `root` in parallel. Blocks until the walk is complete.

- **`root`** — the directory to walk.
- **`config`** — thread count, max depth, symlink behavior.
- **`pre_filter`** — optional cheap filter. Called with borrowed `EntryRef`
  before any `PathBuf` is built. Return `true` to visit, `false` to skip.
  Pass `None` to visit all entries.
- **`visitor_factory`** — called once per thread to produce a per-thread
  visitor. The visitor receives fully materialized `Entry` values.

---

### `WalkConfig`

```rust
pub struct WalkConfig {
    /// Number of worker threads. Defaults to logical CPU count.
    pub threads: usize,

    /// Maximum traversal depth. `None` = unlimited.
    pub max_depth: Option<usize>,

    /// Follow symbolic links. Defaults to false.
    pub follow_links: bool,
}
```

Use `WalkConfig::default()` for sensible defaults (all CPUs, unlimited depth,
no symlink following).

---

### `Entry`

```rust
pub struct Entry {
    /// Full path to the entry. Only materialized if it passed the pre-filter.
    pub path: PathBuf,

    /// What kind of entry this is.
    pub kind: EntryKind,

    /// Depth from root. Root's direct children = 1.
    pub depth: usize,
}
```

---

### `EntryRef`

```rust
pub struct EntryRef<'a> {
    /// Filename only — zero allocation, borrowed from the OS.
    pub name: &'a OsStr,

    /// Depth from root.
    pub depth: usize,

    /// Entry kind.
    pub kind: EntryKind,
}
```

Used in the pre-filter. Gives you the filename and kind without allocating
a full path. Use this to decide whether to materialize the entry.

---

### `EntryKind`

```rust
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}
```

---

## Examples

### Collect all entries into a Vec

```rust
use parawalk::{walk, WalkConfig, Entry, EntryRef};
use std::sync::{Arc, Mutex};

let results = Arc::new(Mutex::new(Vec::<Entry>::new()));

walk(
    "/usr".into(),
    WalkConfig::default(),
    None::<fn(&EntryRef<'_>) -> bool>,
    move || {
        let r = Arc::clone(&results);
        move |entry: Entry| { r.lock().unwrap().push(entry); }
    },
);
```

### Send entries over a channel (recommended for pipelines)

```rust
use parawalk::{walk, WalkConfig, Entry, EntryRef};
use std::sync::mpsc;

let (tx, rx) = mpsc::channel();

walk(
    "/home".into(),
    WalkConfig::default(),
    None::<fn(&EntryRef<'_>) -> bool>,
    move || {
        let tx = tx.clone();
        move |entry: Entry| { let _ = tx.send(entry); }
    },
);

for entry in rx {
    println!("{}", entry.path.display());
}
```

### Pre-filter by filename (zero allocation for non-matches)

```rust
use parawalk::{walk, WalkConfig, Entry, EntryRef};
use std::sync::mpsc;

let (tx, rx) = mpsc::channel();

walk(
    "/home".into(),
    WalkConfig::default(),
    Some(|entry: &EntryRef<'_>| {
        entry.name.to_string_lossy().ends_with(".rs")
    }),
    move || {
        let tx = tx.clone();
        move |entry: Entry| { let _ = tx.send(entry); }
    },
);
```

### Limit depth

```rust
use parawalk::{walk, WalkConfig, Entry, EntryRef};
use std::sync::mpsc;

let (tx, rx) = mpsc::channel();

walk(
    "/home".into(),
    WalkConfig { max_depth: Some(2), ..WalkConfig::default() },
    None::<fn(&EntryRef<'_>) -> bool>,
    move || {
        let tx = tx.clone();
        move |entry: Entry| { let _ = tx.send(entry); }
    },
);
```

### Per-thread batching (high-throughput pipelines)

The visitor factory pattern makes per-thread batching trivial — no locking needed:

```rust
use parawalk::{walk, WalkConfig, Entry, EntryRef};
use std::sync::mpsc;

const BATCH_SIZE: usize = 128;

let (tx, rx) = mpsc::channel::<Vec<Entry>>();

walk(
    "/".into(),
    WalkConfig::default(),
    None::<fn(&EntryRef<'_>) -> bool>,
    move || {
        let tx = tx.clone();
        let mut batch = Vec::with_capacity(BATCH_SIZE);

        move |entry: Entry| {
            batch.push(entry);
            if batch.len() >= BATCH_SIZE {
                let _ = tx.send(std::mem::take(&mut batch));
                batch = Vec::with_capacity(BATCH_SIZE);
            }
        }
    },
);

// Flatten batches on the receiving end
for entry in rx.into_iter().flatten() {
    println!("{}", entry.path.display());
}
```

---

## Design Notes

### Why a factory instead of a shared visitor?

A shared visitor would require `Arc<Mutex<V>>` to be called safely from multiple
threads. The lock becomes a bottleneck at high entry counts. By calling the
factory once per thread, each thread gets its own visitor with zero sharing —
no lock, no contention.

This pattern mirrors `ignore::WalkParallel::run(|| Box::new(...))`.

### Why pre-filter on `&OsStr` instead of `&Path`?

Building a full `PathBuf` requires joining parent + filename — an allocation
every time. `&OsStr` is a zero-cost borrow directly from the OS `readdir`
result. For workloads where most entries are skipped (e.g. pattern matching),
this eliminates the dominant allocation in the hot path.

### Why not use Rayon?

Rayon's work-stealing scheduler has higher per-task overhead than
crossbeam-deque's hand-rolled implementation, which is what `ignore` uses
internally. For IO-bound directory traversal with many small tasks, the
lighter scheduler wins.

---

## What parawalk does NOT do

- Parse `.gitignore`, `.ignore`, or any filter files
- Apply hidden-file rules
- Filter by file type, extension, or glob pattern
- Index or cache results
- Follow symlinks by default (opt-in via `WalkConfig`)

All of the above belong in the caller.
