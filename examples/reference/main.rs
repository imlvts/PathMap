//! An executable reference model of the `pathmap` trie and zipper API, plus a
//! trace front end for differential testing against the Lean specification.
//!
//! This directory is a port of the Lean 4 specification in `lean/PathMapModel/`
//! — see `lean/README.md` for the reasoning behind it and `lean/FINDINGS.md` for
//! what it found.  Every item names the `pathmap` item it specifies, and the file
//! layout mirrors the Lean one one-for-one, so drift between the two is visible
//! as a diff rather than hidden inside a module:
//!
//! | Lean                     | here            |
//! |--------------------------|-----------------|
//! | `Basic.lean`             | `basic.rs`      |
//! | `PathMap.lean`           | `pathmap.rs`    |
//! | `Zipper.lean`            | `zipper.rs`     |
//! | `Write.lean`             | `write.rs`      |
//! | `Map.lean`               | `map.rs`        |
//! | `Spec.lean` §2           | `laws.rs`       |
//! | `Check.lean`             | `check.rs`      |
//! | `Fuzz.lean`              | this file       |
//!
//! It lives in `examples/` rather than in `src/` because nothing in the library
//! may depend on it: it is a testing oracle, not a data structure, and keeping it
//! out of the crate proper is what makes "the model shares no code with the
//! implementation" a fact about the build rather than a promise in a comment.
//! That also means it needs no cargo feature to gate it out of a release build.
//!
//! Run it as `cargo run --release --example reference -- <input-file>`; its unit
//! tests run under `cargo test --example reference` (the target sets
//! `test = true`).
//!
//! # This is not a trie, and that is the point
//!
//! A trie is a prefix tree, which `pathmap` implements with four node types and
//! a great deal of care.  The model is a flat `BTreeMap<Vec<u8>, Option<V>>` of
//! whole paths.  It is not a prefix tree and does not try to be.
//!
//! That is deliberate.  This is a *specification*, and what it specifies is the
//! meaning a trie carries, not the trie.  A model shaped like the implementation
//! would inherit the implementation's structure, and then a bug in how that
//! structure is handled — a node type promoted wrongly, a child index off by
//! one, an empty node where a real one was expected — could be present in both
//! and cancel out.  Findings 14, 15 and 16 are bugs of exactly that kind, and
//! they are visible only because the model has no nodes to get wrong.
//!
//! **The model must therefore never reach into the crate.**  No file in this
//! directory may `use pathmap::...`; not `utils::ByteMask`, not
//! `ring::AlgebraicStatus`, not a helper.  Shared code is shared risk.  The
//! comparison between model and crate belongs in the harness, not here — and the
//! model half of that harness, below, does not touch the crate either.
//!
//! # Why `BTreeMap<Vec<u8>, Option<V>>`
//!
//! The Lean model keeps a list of `(Path, Option V)` pairs held canonical by
//! hand: sorted by the lexicographic path order, duplicate-free, prefix-closed,
//! and containing the empty path.  Rust's `Ord for Vec<u8>` **is** that order —
//! `[] < [0] < [0,0] < [0,255] < [1]` — which is the order a depth-first
//! traversal of a radix trie visits paths in.  So a `BTreeMap` discharges
//! "sorted" and "duplicate-free" structurally, its iteration order is depth-first
//! order for free, and every "the least existing path after the focus such
//! that ..." in the specification becomes a range query.  Prefix-closure and the
//! presence of the root remain the model's own responsibility;
//! `PathMap::mk` establishes them and `laws::prefix_closed` checks them.
//!
//! Structural equality of two canonical maps is therefore observational
//! equality, which is what lets the model decide `AlgebraicStatus::Identity`
//! against `Element` (see `PathMap::beq_t`).
//!
//! # Deviations from the Lean model
//!
//! * The Lean model is purely functional: every operation returns a new `Zip`.
//!   Here the mutating operations take `&mut self` and return only what the
//!   corresponding `pathmap` method returns, so the trace front end can call the
//!   two side by side.  Where a law needs the prior state, it clones.
//! * `Zip` owns its `PathMap` (as in Lean, where a read zipper holds a snapshot
//!   and a write zipper holds the live map).  Operations that read one zipper
//!   and write another take the source by reference.
//! * The proved theorems of `Spec.lean` §1 have no counterpart: they are proofs,
//!   not tests.  The checkable laws of §2 are in `laws.rs`.
//! * `V: Clone` is required throughout; the Lean model is generic over any `V`.
//!
//! # The trace front end
//!
//!     cargo run --release --example reference -- <input-file>   # or stdin
//!     cargo run --release --example reference -- --act <file>   # ACT-mode skips
//!
//! There are three front ends over one wire format:
//!
//! | binary | drives |
//! |---|---|
//! | `lean/.lake/build/bin/pathmap-oracle` | the Lean model (`lean/PathMapModel/Fuzz.lean`) |
//! | `examples/pathmap_trace.rs`           | the real crate (`examples/common/harness.rs`) |
//! | this                                  | the Rust model in this directory |
//!
//! `lean/differential.py` diffs any two of them.  Model-against-model —
//! `--model` — is the acceptance test for this port: the two are independent
//! transcriptions of the same specification in different languages, so a diff
//! means one of them is wrong and nothing about the crate is in question.
//!
//! The rest of this file is a transcription of `Fuzz.lean` and must track it byte
//! for byte: the same operand decoding in the same order (including bytes
//! consumed before an operation decides to `skip`), the same operation table, and
//! the same rendering.  `NOPS` and `MAX_STEPS` are the same contract
//! `examples/common/harness.rs` names.

// The model is a specification: it defines the whole API surface whether or not
// this particular front end happens to call each item.  `laws` and `map` are
// reached only from `check.rs`, under `cfg(test)`.
#![allow(dead_code)]

mod basic;
mod pathmap;
mod zipper;
mod write;
mod map;
mod laws;

#[cfg(test)]
mod check;



mod fuzz;

use fuzz::run;

// so sharing it with the crate's front ends does not share any semantics.
include!("../common/server.rs");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let act = args.iter().any(|a| a == "--act");
    // Resident mode: one process, many inputs over stdin.  See `serve`.
    if args.iter().any(|a| a == "--server") {
        serve(act, run);
        return;
    }
    let file = args.into_iter().find(|a| a != "--act" && a != "--server");
    let bytes: Vec<u8> = match file {
        Some(p) => std::fs::read(p).expect("cannot read input"),
        None => {
            use std::io::Read;
            let mut v = Vec::new();
            std::io::stdin().read_to_end(&mut v).unwrap();
            v
        }
    };
    print!("{}", run(&bytes, act));
}
