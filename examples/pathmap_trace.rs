//! Prints the differential trace for a fuzzer input, from the real crate.
//!
//!     pathmap_trace <input-file>      # or read the bytes from stdin
//!
//! `lean/.lake/build/bin/pathmap-oracle` prints the same trace from the Lean
//! model; `lean/differential.py` runs both and diffs them.

use pathmap::PathMap;
use pathmap::ring::AlgebraicStatus;
use pathmap::utils::ByteMask;
use pathmap::zipper::*;

include!("common/harness.rs");

fn main() {
    // `--check` also asserts the structural invariants the libFuzzer target
    // checks, which is handy for replaying a corpus without cargo-fuzz.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    let file = args.into_iter().find(|a| a != "--check");
    let bytes: Vec<u8> = match file {
        Some(p) => std::fs::read(p).expect("cannot read input"),
        None => {
            use std::io::Read;
            let mut v = Vec::new();
            std::io::stdin().read_to_end(&mut v).unwrap();
            v
        }
    };
    for line in run(&bytes, check) {
        println!("{line}");
    }
}

