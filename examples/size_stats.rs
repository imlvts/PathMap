//! Memory footprint report for `PathMap<()>` over several datasets.
//!
//! Run with `cargo run --release --features counters --example size_stats [dataset ...]`.
//! With no arguments every dataset is measured. Datasets are read from `benches/` (or
//! `BENCH_DATA_DIR`). Each run prints the structural [pathmap::size_stats::SizeStats]
//! report and the live-heap delta seen by a counting global allocator, so the structural
//! model can be checked against what the allocator actually handed out.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use pathmap::PathMap;
use pathmap::size_stats::SizeStats;

#[cfg_attr(feature = "jemalloc", allow(dead_code))]
struct Counting;
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BLOCKS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Relaxed);
        LIVE_BLOCKS.fetch_add(1, Relaxed);
        TOTAL_ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        LIVE_BLOCKS.fetch_sub(1, Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size, Relaxed);
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        TOTAL_ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

// The `jemalloc` feature installs the crate's own global allocator, so the counting one steps aside
// and only the RSS figures remain meaningful.
#[cfg(not(feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Counting = Counting;

#[derive(Clone, Copy)]
struct HeapSnapshot { bytes: usize, blocks: usize, total_allocs: usize, rss_kb: usize }

fn snapshot() -> HeapSnapshot {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let rss_pages: usize = statm.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    HeapSnapshot {
        bytes: LIVE_BYTES.load(Relaxed),
        blocks: LIVE_BLOCKS.load(Relaxed),
        total_allocs: TOTAL_ALLOCS.load(Relaxed),
        rss_kb: rss_pages * 4,
    }
}

fn data_dir() -> PathBuf {
    match std::env::var("BENCH_DATA_DIR") {
        Ok(val) => PathBuf::from(val),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benches"),
    }
}

fn lines(name: &str, limit: usize) -> Vec<Vec<u8>> {
    let file = File::open(data_dir().join(name)).unwrap_or_else(|e| panic!("open {name}: {e}"));
    BufReader::new(file).split(b'\n').map(|l| l.unwrap()).filter(|l| !l.is_empty()).take(limit).collect()
}

fn shakespeare(as_words: bool) -> Vec<Vec<u8>> {
    const TERMINATORS: &[char] = &[',', '.', ';', '?', '\"', '-', '[', ']'];
    const SEPARATORS: &[char] = &[' ', '\t', '\n'];
    let file = File::open(data_dir().join("shakespeare.txt")).unwrap();
    let mut out = vec![];
    for line in BufReader::new(file).lines() {
        let line = line.unwrap();
        for clause in line.split_inclusive(TERMINATORS) {
            if as_words {
                for sym in clause.split(SEPARATORS) {
                    let sym: String = sym.chars().filter(|c| !TERMINATORS.contains(c) && *c != '\'').collect();
                    if !sym.is_empty() { out.push(sym.into_bytes()); }
                }
            } else {
                out.push(clause.as_bytes().to_vec());
            }
        }
    }
    out
}

fn cities_names() -> Vec<Vec<u8>> {
    lines("cities5000.txt", usize::MAX).into_iter()
        .filter_map(|l| l.split(|b| *b == b'\t').nth(2).map(|s| s.to_vec()))
        .collect()
}

/// Splitmix64, so the synthetic sets need no extra dependency
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

fn dataset(name: &str) -> Option<Vec<Vec<u8>>> {
    let keys = match name {
        "cities" => cities_names(),
        "shakespeare_words" => shakespeare(true),
        "shakespeare_sentences" => shakespeare(false),
        "oeis" => lines("oeis_stripped.txt", 100_000).into_iter().filter(|l| l[0] != b'#').collect(),
        "seq_u64" => (0u64..1_000_000).map(|i| i.to_be_bytes().to_vec()).collect(),
        "rand_u64" => { let mut s = 1; (0..1_000_000).map(|_| splitmix(&mut s).to_be_bytes().to_vec()).collect() }
        "rand_16b" => { let mut s = 2; (0..500_000).map(|_| [splitmix(&mut s).to_be_bytes(), splitmix(&mut s).to_be_bytes()].concat()).collect() }
        "rand_32b" => { let mut s = 3; (0..250_000).map(|_| (0..4).flat_map(|_| splitmix(&mut s).to_be_bytes()).collect()).collect() }
        "rand_128b" => { let mut s = 4; (0..100_000).map(|_| (0..16).flat_map(|_| splitmix(&mut s).to_be_bytes()).collect()).collect() }
        _ => return None,
    };
    Some(keys)
}

const DATASETS: &[&str] = &["cities", "shakespeare_words", "shakespeare_sentences", "oeis", "big_logic_paths", "seq_u64", "rand_u64", "rand_16b", "rand_32b", "rand_128b"];

fn build(name: &str) -> (PathMap<()>, usize, usize) {
    if name == "big_logic_paths" {
        let file = File::open(data_dir().join("big_logic.metta.paths")).unwrap();
        let mut map = PathMap::<()>::new();
        let mut path_bytes = 0;
        let mut count = 0;
        pathmap::paths_serialization::for_each_deserialized_path(BufReader::new(file), |_, p| { path_bytes += p.len(); count += 1; map.set_val_at(p, ()); Ok(()) }).unwrap();
        return (map, count, path_bytes);
    }
    let keys = dataset(name).unwrap_or_else(|| panic!("unknown dataset {name}; known: {DATASETS:?}"));
    let path_bytes = keys.iter().map(|k| k.len()).sum();
    let count = keys.len();
    let mut map = PathMap::<()>::new();
    for k in &keys {
        map.set_val_at(k, ());
    }
    drop(keys);
    (map, count, path_bytes)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let names: Vec<&str> = if args.is_empty() { DATASETS.to_vec() } else { args.iter().map(|s| s.as_str()).collect() };
    let mut summary = vec![];
    let mut projection_rows: Vec<(String, Vec<pathmap::size_stats::Projection>)> = vec![];
    for name in names {
        let before = snapshot();
        let (map, input_count, input_bytes) = build(name);
        let after = snapshot();
        let stats = SizeStats::measure(&map);
        let live_bytes = after.bytes - before.bytes;
        let live_blocks = after.blocks - before.blocks;
        println!("=== {name}: {input_count} input paths, {input_bytes} input bytes ({} distinct values) ===", map.val_count());
        println!("heap delta: {live_bytes} B live in {live_blocks} blocks, {} alloc calls during build, RSS +{} kB",
            after.total_allocs - before.total_allocs, after.rss_kb.saturating_sub(before.rss_kb));
        println!("structural: {} B requested in {} blocks ({:.2}% of heap delta), {} B malloc est ({:.1} B/input byte)",
            stats.total_requested_bytes(), stats.total_allocations(), 100.0 * stats.total_requested_bytes() as f64 / live_bytes.max(1) as f64,
            stats.total_malloc_bytes(), stats.total_malloc_bytes() as f64 / input_bytes.max(1) as f64);
        print!("{}", stats.report());
        print!("{}", stats.projection_report());
        println!();
        projection_rows.push((name.to_string(), stats.projections()));
        summary.push((name.to_string(), stats.value_count, stats.total_nodes(), stats.list.nodes,
            stats.dense.nodes + stats.cell.nodes + stats.sparse.nodes + stats.unit.nodes,
            live_bytes, stats.total_requested_bytes(), stats.total_useful_bytes(), stats.total_unused_bytes(),
            stats.list_chain_nodes, stats.list_leaf_nodes));
        drop(map);
    }
    println!("{:<22} {:>9} {:>9} {:>9} {:>11} {:>12} {:>12} {:>12} {:>12} {:>8} {:>8}", "dataset", "values", "nodes", "list", "byte_nodes", "heap_bytes", "requested", "useful", "unused", "chain%", "leaf%");
    for (name, values, nodes, list, byte_nodes, heap, requested, useful, unused, chain, leaf) in &summary {
        println!("{:<22} {:>9} {:>9} {:>9} {:>11} {:>12} {:>12} {:>12} {:>12} {:>7.1}% {:>7.1}%", name, values, nodes, list, byte_nodes, heap, requested, useful, unused,
            100.0 * *chain as f64 / (*list).max(1) as f64, 100.0 * *leaf as f64 / (*list).max(1) as f64);
    }
    if let Some((_, first)) = projection_rows.first() {
        println!();
        println!("projected requested bytes as % of the measured layout (see SizeStats::projections for what each P* assumes)");
        print!("{:<22} {:>12}", "dataset", "measured_B");
        for p in first.iter().skip(1) {
            print!(" {:>12}", p.name.split(' ').take(2).collect::<Vec<_>>().join(" "));
        }
        println!();
        for (name, projections) in &projection_rows {
            let base = projections[0].requested_bytes.max(1);
            print!("{:<22} {:>12}", name, projections[0].requested_bytes);
            for p in projections.iter().skip(1) {
                print!(" {:>11.1}%", 100.0 * p.requested_bytes as f64 / base as f64);
            }
            println!();
        }
    }
}
