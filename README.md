# parawalk

Blazing-fast parallel directory walker with zero filtering baggage.

parawalk is a minimal parallel filesystem traversal library. It uses a
work-stealing scheduler (crossbeam-deque) to walk directory trees across
multiple threads, giving each thread its own visitor so there's no shared
state or locking in the hot path.

**parawalk is not a drop-in replacement for the `ignore` crate.**
It does not parse `.gitignore` files, glob patterns, or apply any filtering
rules. It focuses solely on fast parallel traversal — filtering and matching
are left to the caller.

## Features

- Work-stealing parallel traversal (crossbeam-deque)
- Per-thread visitors — no `Mutex`, no contention
- Pre-filter hook on borrowed `&OsStr` — zero allocation for skipped entries
- PathBuf only materialized for entries that pass the pre-filter
- Configurable thread count, max depth, and symlink following
- `#![forbid(unsafe_code)]`

## Quick Start

```toml
[dependencies]
parawalk = "0.1"
```

```rust
use parawalk::{walk, WalkConfig, Entry, EntryRef};
use std::sync::mpsc;

let (tx, rx) = mpsc::channel();

walk(
    "/usr".into(),
    WalkConfig::default(),
    None::<fn(&EntryRef<'_>) -> bool>,
    move || {
        let tx = tx.clone();
        move |entry: Entry| { let _ = tx.send(entry); }
    },
);

let count = rx.into_iter().count();
println!("Found {} entries", count);
```

## Usage

See [DOCS.md](DOCS.md) for the full API reference and usage guide.

## License

MIT
