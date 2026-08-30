//! In-process differential fuzzer: the Rust reference model against the real crate.
//!
//!     cargo run --release --example differential -- --random 100000
//!     cargo run --release --example differential -- --random 1000000 -j 64
//!     cargo run --release --example differential -- corpus/*.bin
//!
//! `lean/differential.py` compares the *Lean* model against something else, and
//! pays for it: two child processes, a hex-encoded input over a pipe, a rendered
//! trace back, and a Python driver in the middle.  That buys language
//! independence -- two transcriptions of one specification, written in different
//! languages -- and it is the right tool for validating the port.
//!
//! It is the wrong tool for *volume*.  Once the Rust model is known to agree with
//! the Lean one, the crate can be checked against the Rust model with no
//! processes, no pipes and no serialisation at all: both run in this binary, on
//! the same input, and the results are compared in memory.  Nothing is rendered
//! unless something diverges.
//!
//! # What is being compared
//!
//! Exactly what `differential.py` compares, through exactly the same two op
//! tables -- there is no fourth transcription here:
//!
//! * the model side is `examples/reference/fuzz.rs`, the port of `Fuzz.lean`;
//! * the crate side is `examples/common/harness.rs`, shared with
//!   `pathmap_trace` and the libFuzzer target.
//!
//! Both are driven from the same bytes and must produce the same trace.  Sharing
//! the op tables rather than copying them is the point: a fourth copy would drift.
//!
//! # The model does not touch the crate
//!
//! The files under `examples/reference/` still may not `use pathmap::...`; that
//! rule is what makes "the model shares no code with the implementation" a fact
//! about the build.  This file is the one place both are in scope, and it is a
//! comparator, not a model: the extern crate is spelled `::pathmap` here because
//! the model's own map module is called `pathmap` too.

// The model is a specification: it defines the whole API surface whether or not
// this comparator happens to call each item.
#![allow(dead_code)]

// The model.  Same files the `reference` example builds, included by path rather
// than copied, so the two binaries cannot drift.
#[path = "../reference/basic.rs"]
mod basic;
#[path = "../reference/pathmap.rs"]
mod pathmap;
#[path = "../reference/zipper.rs"]
mod zipper;
#[path = "../reference/write.rs"]
mod write;
#[path = "../reference/map.rs"]
mod map;
#[path = "../reference/laws.rs"]
mod laws;
#[path = "../reference/fuzz.rs"]
mod fuzz;

// The crate.  `::pathmap` is explicit because `mod pathmap` above is the model's.
use ::pathmap::PathMap;
use ::pathmap::ring::AlgebraicStatus;
use ::pathmap::utils::ByteMask;
use ::pathmap::zipper::*;

// The crate-side op table, shared with `pathmap_trace` and the fuzz target.  It
// defines `run`, which is the crate's half of the comparison.
include!("../common/harness.rs");

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Where inputs come from.
///
/// `get` is deterministic in `idx` and holds no state between calls, so which
/// thread runs an input cannot change it and a failing input can be re-derived
/// from its index alone.  Mirrors `InputSource` in `lean/differential.py`; a
/// queue-backed source drops in here the same way.
enum Source {
    Random { seed: u64, count: usize, maxlen: usize },
    Files(Vec<String>),
}

impl Source {
    fn len(&self) -> usize {
        match self {
            Source::Random { count, .. } => *count,
            Source::Files(v) => v.len(),
        }
    }

    fn name(&self, idx: usize) -> String {
        match self {
            Source::Random { .. } => format!("random#{idx:06}"),
            Source::Files(v) => v[idx].clone(),
        }
    }

    fn get(&self, idx: usize) -> Vec<u8> {
        match *self {
            Source::Random { seed, maxlen, .. } => {
                // splitmix64, seeded per index so generation parallelises without
                // changing what gets tested.
                let mut s = seed
                    .wrapping_mul(0x9E3779B97F4A7C15)
                    ^ (idx as u64).wrapping_mul(0xBF58476D1CE4E5B9);
                let mut next = || {
                    s = s.wrapping_add(0x9E3779B97F4A7C15);
                    let mut z = s;
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                    z ^ (z >> 31)
                };
                let n = 8 + (next() as usize) % maxlen;
                (0..n).map(|_| next() as u8).collect()
            }
            Source::Files(ref v) => std::fs::read(&v[idx]).expect("cannot read input"),
        }
    }
}

/// The index each thread is currently working on, so an abort can be attributed.
///
/// A panic in the crate is a divergence and is caught; an *abort* is not
/// catchable in-process and takes the fuzzer with it.  The crate does abort on
/// some inputs -- a 1,000,000-input pass turns up heap corruption
/// (`malloc(): unaligned tcache chunk detected`) -- so the fuzzer prints what
/// every thread had in flight on the way down, and `--from` resumes past it.
/// That is the price of running in one process, and it is worth paying: the
/// subprocess design absorbs an abort but costs 4x the throughput.
static IN_FLIGHT: [AtomicUsize; 256] = [const { AtomicUsize::new(usize::MAX) }; 256];

/// Compare one input.  Returns the rendered report on divergence, `None` on
/// agreement.
///
/// The traces are only *rendered* because both op tables render them today; the
/// comparison itself is a `Vec<String>` equality, and neither side leaves this
/// process.  See the note in `main` about what rendering still costs.
fn compare(blob: &[u8]) -> Option<String> {
    let model = fuzz::run(blob, false);
    // NOT wrapped in `catch_unwind`, and that is deliberate.  `pathmap` is not
    // unwind-safe: catching a panic out of the middle of a trie mutation and
    // carrying on corrupts the heap, because the half-updated refcounted nodes
    // are then dropped.  Measured, not assumed -- input 34380 of seed 5 panics
    // the crate (`write_zipper.rs:2823`, finding `ascend_until_wz`), and catching
    // that panic turns a clean `rc=101` into
    // `malloc(): unaligned tcache chunk detected`.  So the panic hook installed
    // in `main` reports and exits instead of letting the stack unwind at all.
    let real = run(blob, false);
    // The fast path is one `memcmp` over two buffers.  Individual lines are only
    // needed to *report* a divergence, so they are only split out then.
    if model == real {
        return None;
    }
    for (i, (a, b)) in model.lines().zip(real.lines()).enumerate() {
        if a != b {
            return Some(format!("line {i}\n  model: {a}\n  crate: {b}"));
        }
    }
    Some(format!(
        "length {} (model) vs {} (crate) lines",
        model.lines().count(),
        real.lines().count()
    ))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str, default: usize| -> usize {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let count = flag("--random", 0);
    // Index to start at.  Inputs are deterministic in their index, so a run that
    // dies can be resumed past the offending one -- see the note below on aborts.
    let from = flag("--from", 0);
    let seed = flag("--seed", 1) as u64;
    let maxlen = flag("--maxlen", 300);
    let jobs = flag("-j", 1).max(1);
    let max_fails = flag("--max-fails", 10);
    let files: Vec<String> = {
        let mut v = Vec::new();
        let mut it = args.iter().peekable();
        while let Some(a) = it.next() {
            if a.starts_with('-') {
                it.next(); // its value
            } else {
                v.push(a.clone());
            }
        }
        v
    };
    let source = if !files.is_empty() {
        Source::Files(files)
    } else if count > 0 {
        Source::Random { seed, count, maxlen }
    } else {
        eprintln!("usage: differential [--random N] [--seed S] [--maxlen L] [-j N] [FILES...]");
        std::process::exit(2);
    };

    // `--dump IDX` writes one input to stdout and exits, so an input that aborts
    // the process can still be extracted and replayed against the crate alone.
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let idx: usize = args[i + 1].parse().expect("--dump IDX");
        use std::io::Write;
        std::io::stdout().write_all(&source.get(idx)).unwrap();
        return;
    }

    let n = source.len();
    let next = AtomicUsize::new(from);
    let agreed = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let reports: Mutex<Vec<(usize, String, String)>> = Mutex::new(Vec::new());
    // A crate panic ends the run, by design: see `compare`.  The hook runs
    // *before* the stack unwinds, so it is the last safe moment to say which
    // input did it -- and exiting from here means nothing unwinds through
    // `pathmap`'s internals.
    std::panic::set_hook(Box::new(|info| {
        let live: Vec<usize> = IN_FLIGHT
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .filter(|&i| i != usize::MAX)
            .collect();
        eprintln!("CRATE PANIC on input index {live:?}: {info}");
        eprintln!("  the crate panics on this input; the model is total and cannot.");
        eprintln!("  resume past it with --from {}", live.iter().max().map_or(0, |m| m + 1));
        // Not `abort`: exit runs no destructors and unwinds nothing.
        std::process::exit(101);
    }));

    // An abort inside the crate cannot be caught, so leave a breadcrumb: the
    // indices in flight when it happened, which `--from` can resume past.
    let hint = std::panic::catch_unwind(|| ());
    let _ = hint;
    let start = std::time::Instant::now();
    let (src, next_r, stop_r, agreed_r, reports_r) = (&source, &next, &stop, &agreed, &reports);
    std::thread::scope(|scope| {
        for slot in 0..jobs {
            scope.spawn(move || {
                loop {
                    if stop_r.load(Ordering::Relaxed) {
                        return;
                    }
                    let idx = next_r.fetch_add(1, Ordering::Relaxed);
                    if idx >= n {
                        IN_FLIGHT[slot.min(255)].store(usize::MAX, Ordering::Relaxed);
                        return;
                    }
                    IN_FLIGHT[slot.min(255)].store(idx, Ordering::Relaxed);
                    match compare(&src.get(idx)) {
                        None => {
                            agreed_r.fetch_add(1, Ordering::Relaxed);
                        }
                        Some(msg) => {
                            let mut r = reports_r.lock().unwrap();
                            r.push((idx, src.name(idx), msg));
                            if max_fails != 0 && r.len() >= max_fails {
                                stop_r.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
            });
        }
    });
    let elapsed = start.elapsed().as_secs_f64();

    // Sorted by input index, so a -j run reports in the same order a -j1 run does.
    let mut reports = reports.into_inner().unwrap();
    reports.sort_by_key(|(idx, _, _)| *idx);
    for (idx, name, msg) in &reports {
        let path = std::env::temp_dir().join(format!("differential-{idx:06}.bin"));
        let _ = std::fs::write(&path, source.get(*idx));
        println!("FAIL {name} [saved {}]: {msg}", path.display());
    }
    let in_flight: Vec<usize> = IN_FLIGHT
        .iter()
        .map(|a| a.load(Ordering::Relaxed))
        .filter(|&i| i != usize::MAX)
        .collect();
    if !in_flight.is_empty() {
        eprintln!("(threads still had {in_flight:?} in flight)");
    }
    let ok = agreed.load(Ordering::Relaxed);
    let done = ok + reports.len();
    println!(
        "{ok}/{done} inputs agree ({} divergences) in {elapsed:.2}s -> {:.0} inputs/s",
        reports.len(),
        done as f64 / elapsed
    );
    if !reports.is_empty() {
        std::process::exit(1);
    }
}
