use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use parawalk::{Entry, WalkConfig, walk};

#[test]
fn walks_tmp_dir() {
    // Create a small temp tree
    let tmp = std::env::temp_dir().join("parawalk_test");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("a.txt"), "a").unwrap();
    std::fs::write(tmp.join("b.txt"), "b").unwrap();
    let sub = tmp.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("c.txt"), "c").unwrap();

    let results: Arc<Mutex<Vec<Entry>>> = Arc::new(Mutex::new(Vec::new()));
    let results_clone = Arc::clone(&results);

    walk(
        tmp.clone(),
        WalkConfig::default(),
        None::<fn(&parawalk::EntryRef<'_>) -> bool>,
        move || {
            let r = Arc::clone(&results_clone);
            move |entry| { r.lock().unwrap().push(entry); }
        },
    );

    let found = results.lock().unwrap();
    assert_eq!(found.len(), 4); // a.txt, b.txt, sub/, sub/c.txt

    // Cleanup
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn pre_filter_skips_non_matching() {
    let tmp = std::env::temp_dir().join("parawalk_filter_test");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("invoice.txt"), "").unwrap();
    std::fs::write(tmp.join("report.txt"), "").unwrap();
    std::fs::write(tmp.join("invoice_feb.txt"), "").unwrap();

    let results: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let results_clone = Arc::clone(&results);

    walk(
        tmp.clone(),
        WalkConfig::default(),
        Some(|entry: &parawalk::EntryRef<'_>| {
            entry.name.to_string_lossy().contains("invoice")
        }),
        move || {
            let r = Arc::clone(&results_clone);
            move |entry: parawalk::Entry| { r.lock().unwrap().push(entry.path); }
        },
    );

    let found = results.lock().unwrap();
    assert_eq!(found.len(), 2);

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn respects_max_depth() {
    let tmp = std::env::temp_dir().join("parawalk_depth_test");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("root.txt"), "").unwrap();
    let sub = tmp.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("deep.txt"), "").unwrap();

    let results: Arc<Mutex<Vec<Entry>>> = Arc::new(Mutex::new(Vec::new()));
    let results_clone = Arc::clone(&results);

    walk(
        tmp.clone(),
        WalkConfig { max_depth: Some(1), ..WalkConfig::default() },
        None::<fn(&parawalk::EntryRef<'_>) -> bool>,
        move || {
            let r = Arc::clone(&results_clone);
            move |entry| { r.lock().unwrap().push(entry); }
        },
    );

    let found = results.lock().unwrap();
    // depth 1 = root.txt + sub/ only, not sub/deep.txt
    assert_eq!(found.len(), 2);

    std::fs::remove_dir_all(&tmp).ok();
}
