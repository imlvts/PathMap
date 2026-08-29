//! libFuzzer target for the `pathmap` zipper API.
//!
//! Shares its decoder with `examples/pathmap_trace.rs` and with the Lean model
//! in `lean/PathMapModel/Fuzz.lean`, so the corpus this target produces can be
//! replayed against the model directly:
//!
//! ```text
//! cargo +nightly fuzz run zipper_ops
//! ./lean/differential.py fuzz/corpus/zipper_ops/*
//! ```
//!
//! In-process this target only checks *structural invariants* (see
//! `check_zipper`), which needs no oracle and so runs at full fuzzing speed.
//! The deeper, semantic comparison — every return value and the whole final
//! trie, against the Lean model — is what `differential.py` adds afterwards.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pathmap::PathMap;
use pathmap::ring::AlgebraicStatus;
use pathmap::utils::ByteMask;
use pathmap::zipper::*;

include!("../../examples/common/harness.rs");

fuzz_target!(|data: &[u8]| {
    let _ = run(data, true);
});
