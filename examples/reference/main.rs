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

use basic::{AlgStatus, ByteMask, U64Ops, path};
use pathmap::PathMap;
use zipper::Zip;


/// Number of distinct operations.  Must match `PathMapModel.Fuzz.nops` and
/// `NOPS` in `examples/common/harness.rs`.
const NOPS: usize = 56;
/// Maximum operations executed.  Must match the `maxSteps` default in `Fuzz.run`.
const MAX_STEPS: usize = 256;
/// Maximum entries in a `dump`.  Must match `Fuzz.dumpAt`.
const DUMP_CAP: usize = 64;

const OPS: U64Ops = U64Ops;

// ---------------------------------------------------------------------------
// Decoder — `Fuzz.Dec`
// ---------------------------------------------------------------------------

struct Dec<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Dec<'_> {
    /// Read one byte; `None` once the input is exhausted, which ends the program.
    fn u8(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    /// Read one byte reduced modulo `m`.
    fn modn(&mut self, m: usize) -> Option<usize> {
        let b = self.u8()?;
        Some(if m == 0 { 0 } else { (b as usize) % m })
    }
    /// Path bytes live in a 4-letter alphabet so generated tries share prefixes.
    fn path_byte(&mut self) -> Option<u8> {
        Some(self.u8()? % 4)
    }
    fn path_n(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.path_byte()?);
        }
        Some(v)
    }
    /// Read a length-prefixed path (`len := u8 % lim`).
    fn path(&mut self, lim: usize) -> Option<Vec<u8>> {
        let n = self.modn(lim)?;
        self.path_n(n)
    }
    fn boolean(&mut self) -> Option<bool> {
        Some(self.u8()? % 2 == 1)
    }
}

// ---------------------------------------------------------------------------
// Rendering — `Fuzz` §Rendering
// ---------------------------------------------------------------------------

fn hex_path(p: &[u8]) -> String {
    if p.is_empty() {
        "_".to_string()
    } else {
        p.iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn show_val(v: Option<&u64>) -> String {
    match v {
        None => "-".to_string(),
        Some(v) => format!("{v}"),
    }
}

/// Render the `Option<u8>` the movement operations return: the byte moved to, or
/// `-` for "did not move".
fn show_byte_opt(b: Option<u8>) -> String {
    match b {
        None => "-".to_string(),
        Some(b) => format!("{b:02x}"),
    }
}

fn show_bool(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

fn show_status(s: AlgStatus) -> String {
    s.to_string()
}

/// All locations at and below `root`, depth-first, capped so a runaway trie
/// cannot make the trace unbounded.
fn dump_at(t: &PathMap<u64>, root: &[u8]) -> String {
    t.subtrie(root)
        .paths()
        .take(DUMP_CAP)
        .map(|q| format!("{}:{}", hex_path(q), show_val(t.val_at(&path::cat(root, q)))))
        .collect::<Vec<_>>()
        .join(",")
}

// ---------------------------------------------------------------------------
// Interpreter state — `Fuzz.St`
// ---------------------------------------------------------------------------

/// Two maps, two zippers: `wz` writes into map0, `rz` reads map1.  Keeping the
/// read source in a *separate* map is what lets the real crate hold both zippers
/// at once.
struct St {
    wz: Zip<u64>,
    rz: Zip<u64>,
    out: Vec<String>,
    step: usize,
    /// Read source is an `ArenaCompactTree` rather than a `PathMap`, which cannot
    /// be the source of a graft or an algebraic merge.  Those operations report
    /// `skip` on both sides of the comparison.
    act: bool,
}

/// The per-step fingerprint of one zipper.
fn fingerprint(z: &Zip<u64>) -> String {
    format!(
        "{} o{} e{} v{} c{} n{} f{}",
        hex_path(&z.path),
        hex_path(&z.focus()),
        show_bool(z.path_exists()),
        show_val(z.val()),
        z.child_count(),
        z.val_count(),
        // `focus_byte` is unspecified at the root, so it is only compared below it.
        if z.at_root() { "?".to_string() } else { show_byte_opt(z.focus_byte()) }
    )
}

impl St {
    fn emit(&mut self, name: &str, ret: &str) {
        self.out.push(format!(
            "{} {} ret={} W={} R={}",
            self.step,
            name,
            ret,
            fingerprint(&self.wz),
            fingerprint(&self.rz)
        ));
        self.step += 1;
    }

    /// The zipper selected by a target byte (`0` = write zipper, `1` = read).
    fn target(&mut self, t: usize) -> &mut Zip<u64> {
        if t == 0 { &mut self.wz } else { &mut self.rz }
    }

    fn target_ref(&self, t: usize) -> &Zip<u64> {
        if t == 0 { &self.wz } else { &self.rz }
    }

    /// Is the *explicit* `prune_path` / `prune_ascend` well-defined for this
    /// state?  Only for a write zipper rooted at the map root: with a non-empty
    /// root the depth pruned becomes a function of node layout rather than of the
    /// logical trie.  See `Fuzz.pruneable` and lean/FINDINGS.md finding 7.
    fn pruneable(&self) -> bool {
        self.wz.root.is_empty()
    }
}

/// The `prune` flag the harness passes to operations that take one.
///
/// Always `false`: the flag's effect is a function of internal node layout rather
/// than of the logical trie, so there is nothing for a model to agree with.
const NO_PRUNE: bool = false;

/// A full `k`-path iteration: `descend_first_k_path` followed by `to_next_k_path`
/// until it runs out (capped at 32 stops).  This is the only well-defined way to
/// use the k-path primitives.
fn k_walk(z: &mut Zip<u64>, k: usize) -> Vec<Vec<u8>> {
    if !z.descend_first_k_path(k) {
        return Vec::new();
    }
    let mut acc = vec![z.path.clone()];
    for _ in 0..31 {
        if z.to_next_k_path(k) {
            acc.push(z.path.clone());
        } else {
            break;
        }
    }
    acc
}

// ---------------------------------------------------------------------------
// The operation table — `Fuzz.step`
// ---------------------------------------------------------------------------
//
// `op % NOPS` selects the operation.  Ops `0`–`26` act on a target zipper chosen
// by a following `u8 % 2` byte (`0` = write zipper, `1` = read zipper); the rest
// are write-zipper operations.
//
// Operand bytes are consumed *before* an operation decides to `skip`, exactly as
// in `Fuzz.lean`, because the byte stream must stay in step across all three
// front ends.  `None` means the input ran out mid-operation, which ends the run.

fn step(s: &mut St, d: &mut Dec) -> Option<()> {
    let op = d.u8()? as usize % NOPS;
    match op {
        0 => {
            let t = d.modn(2)?;
            let p = d.path(6)?;
            s.target(t).descend_to(&p);
            s.emit("descend_to", &hex_path(&p));
        }
        1 => {
            let t = d.modn(2)?;
            let b = d.path_byte()?;
            s.target(t).descend_to_byte(b);
            s.emit("descend_to_byte", &format!("{b:02x}"));
        }
        2 => {
            let t = d.modn(2)?;
            let n = d.modn(8)?;
            let r = s.target(t).ascend(n);
            s.emit("ascend", &r.to_string());
        }
        3 => {
            let t = d.modn(2)?;
            let r = s.target(t).ascend_byte();
            s.emit("ascend_byte", show_bool(r));
        }
        4 => {
            let t = d.modn(2)?;
            s.target(t).reset();
            s.emit("reset", "-");
        }
        5 => {
            let t = d.modn(2)?;
            let r = s.target(t).descend_first_byte();
            s.emit("descend_first_byte", &show_byte_opt(r));
        }
        6 => {
            let t = d.modn(2)?;
            let r = s.target(t).descend_last_byte();
            s.emit("descend_last_byte", &show_byte_opt(r));
        }
        7 => {
            let t = d.modn(2)?;
            let i = d.modn(6)?;
            let r = s.target(t).descend_indexed_byte(i);
            s.emit("descend_indexed_byte", &show_byte_opt(r));
        }
        8 => {
            let t = d.modn(2)?;
            let r = s.target(t).descend_until();
            s.emit("descend_until", show_bool(r));
        }
        9 => {
            let t = d.modn(2)?;
            let r = s.target(t).ascend_until();
            s.emit("ascend_until", &r.to_string());
        }
        10 => {
            let t = d.modn(2)?;
            let r = s.target(t).ascend_until_branch();
            s.emit("ascend_until_branch", &r.to_string());
        }
        11 => {
            let t = d.modn(2)?;
            // Skipped at the zipper root: `ReadZipper::to_next_sibling_byte`
            // escapes its own root there (see `Zip::to_next_sibling_byte`).
            if s.target_ref(t).at_root() {
                s.emit("to_next_sibling_byte", "skip");
            } else {
                let r = s.target(t).to_next_sibling_byte();
                s.emit("to_next_sibling_byte", &show_byte_opt(r));
            }
        }
        12 => {
            let t = d.modn(2)?;
            if s.target_ref(t).at_root() {
                s.emit("to_prev_sibling_byte", "skip");
            } else {
                let r = s.target(t).to_prev_sibling_byte();
                s.emit("to_prev_sibling_byte", &show_byte_opt(r));
            }
        }
        13 => {
            let t = d.modn(2)?;
            let r = s.target(t).to_next_step();
            s.emit("to_next_step", show_bool(r));
        }
        14 => {
            // `ZipperIteration` is read-only: the target byte is still consumed,
            // but the operation always applies to the read zipper.
            let _t = d.modn(2)?;
            let r = s.rz.to_next_val();
            s.emit("to_next_val", show_bool(r));
        }
        15 => {
            let _t = d.modn(2)?;
            let k = d.modn(4)?;
            // `k = 0` is degenerate: `k_path_internal` treats "already at depth
            // base+0" as a hit and reports success without moving, then
            // `to_next_k_path(0)` reports success forever.  Skipped.
            if k == 0 {
                s.emit("descend_first_k_path", "skip");
            } else {
                let r = s.rz.descend_first_k_path(k);
                s.emit("descend_first_k_path", show_bool(r));
            }
        }
        16 => {
            let _t = d.modn(2)?;
            let k = d.modn(4)?;
            // `to_next_k_path` is only meaningful as the continuation of a
            // `descend_first_k_path` iteration, so the op is the whole walk.
            if k == 0 {
                s.emit("k_path_walk", "skip");
            } else {
                let ps = k_walk(&mut s.rz, k);
                let ret = ps.iter().map(|p| hex_path(p)).collect::<Vec<_>>().join(",");
                s.emit("k_path_walk", &ret);
            }
        }
        17 => {
            let _t = d.modn(2)?;
            let r = s.rz.descend_last_path();
            s.emit("descend_last_path", show_bool(r));
        }
        18 => {
            let t = d.modn(2)?;
            let p = d.path(6)?;
            let n = s.target(t).move_to_path(&p);
            s.emit("move_to_path", &n.to_string());
        }
        19 => {
            let t = d.modn(2)?;
            let p = d.path(6)?;
            let n = s.target(t).descend_to_existing(&p);
            s.emit("descend_to_existing", &n.to_string());
        }
        20 => {
            let t = d.modn(2)?;
            let p = d.path(6)?;
            let n = s.target(t).descend_to_val(&p);
            s.emit("descend_to_val", &n.to_string());
        }
        21 => {
            let t = d.modn(2)?;
            let b = d.path_byte()?;
            let r = s.target(t).descend_to_existing_byte(b);
            s.emit("descend_to_existing_byte", show_bool(r));
        }
        22 => {
            let t = d.modn(2)?;
            let n = d.modn(8)?;
            let r = s.target(t).descend_until_max_bytes(n);
            s.emit("descend_until_max_bytes", show_bool(r));
        }
        23 => {
            let t = d.modn(2)?;
            let p = d.path(6)?;
            let r = s.target(t).descend_to_check(&p);
            s.emit("descend_to_check", show_bool(r));
        }
        24 => {
            let t = d.modn(2)?;
            let p = d.path(6)?;
            let ret = show_val(s.target_ref(t).val_at(&p));
            s.emit("val_at", &ret);
        }
        25 => {
            let t = d.modn(2)?;
            if s.act && t == 1 {
                s.emit("make_map_val_count", "skip");
            } else {
                let ret = s.target_ref(t).make_map().val_count(&[]).to_string();
                s.emit("make_map_val_count", &ret);
            }
        }
        26 => {
            let t = d.modn(2)?;
            let z = s.target_ref(t);
            let ret = dump_at(&z.trie, &z.focus());
            s.emit("dump", &ret);
        }
        27 => {
            let v = d.u8()?;
            let old = s.wz.set_val(v as u64);
            s.emit("set_val", &show_val(old.as_ref()));
        }
        28 => {
            let _pr = d.boolean()?;
            let old = s.wz.remove_val(NO_PRUNE);
            s.emit("remove_val", &show_val(old.as_ref()));
        }
        29 => {
            let r = s.wz.create_path();
            s.emit("create_path", show_bool(r));
        }
        30 => {
            if s.pruneable() {
                let n = s.wz.prune_path();
                s.emit("prune_path", &n.to_string());
            } else {
                s.emit("prune_path", "skip");
            }
        }
        31 => {
            if s.pruneable() {
                let n = s.wz.prune_ascend();
                s.emit("prune_ascend", &n.to_string());
            } else {
                s.emit("prune_ascend", "skip");
            }
        }
        32 => {
            let _pr = d.boolean()?;
            let leaky = s.wz.focus_node_is_empty();
            let r = s.wz.remove_branches(NO_PRUNE);
            let ret = if leaky { "?".to_string() } else { show_bool(r).to_string() };
            s.emit("remove_branches", &ret);
        }
        33 => {
            let n = d.modn(4)?;
            let m = d.path_n(n)?;
            let _pr = d.boolean()?;
            let mask = ByteMask::of_list(m);
            s.wz.remove_unmasked_branches(&mask, NO_PRUNE);
            let ret = hex_path(mask.bytes());
            s.emit("remove_unmasked_branches", &ret);
        }
        34 => {
            if s.act {
                s.emit("graft", "skip");
            } else {
                s.wz.graft(&s.rz);
                s.emit("graft", "-");
            }
        }
        35 => {
            let p = d.path(6)?;
            if s.act {
                s.emit("graft_src_at", "skip");
            } else {
                s.wz.graft_src_at(&s.rz, &p);
                s.emit("graft_src_at", &hex_path(&p));
            }
        }
        36 => {
            if s.act {
                s.emit("join_into", "skip");
            } else {
                let st = s.wz.join_into(&OPS, &s.rz);
                s.emit("join_into", &show_status(st));
            }
        }
        37 => {
            if s.act {
                s.emit("join_map_into", "skip");
            } else {
                let leaky = s.wz.focus_node_is_empty();
                let m = s.rz.make_map();
                let st = s.wz.join_map_into(&OPS, &m);
                let ret = if leaky { "?".to_string() } else { show_status(st) };
                s.emit("join_map_into", &ret);
            }
        }
        38 => {
            let _pr = d.boolean()?;
            if s.act {
                s.emit("meet_into", "skip");
            } else {
                let st = s.wz.meet_into(&OPS, &s.rz, NO_PRUNE);
                s.emit("meet_into", &show_status(st));
            }
        }
        39 => {
            let _pr = d.boolean()?;
            if s.act {
                s.emit("subtract_into", "skip");
            } else {
                let st = s.wz.subtract_into(&OPS, &s.rz, NO_PRUNE);
                s.emit("subtract_into", &show_status(st));
            }
        }
        40 => {
            if s.act {
                s.emit("restrict", "skip");
            } else {
                let leaky = s.wz.focus_node_is_empty();
                let st = s.wz.restrict(&OPS, &s.rz);
                let ret = if leaky { "?".to_string() } else { show_status(st) };
                s.emit("restrict", &ret);
            }
        }
        41 => {
            if s.act {
                s.emit("restricting", "skip");
            } else if s.wz.focus_node_is_empty() || s.rz.focus_node_is_empty() {
                // Skipped, not merely masked: there `restricting` branches on
                // whether an empty node happens to be materialised, and the two
                // branches differ in *effect*, not just in the reported bool.
                s.emit("restricting", "skip");
            } else {
                let r = s.wz.restricting(&s.rz);
                s.emit("restricting", show_bool(r));
            }
        }
        42 => {
            let k = d.modn(4)?;
            let _pr = d.boolean()?;
            // `join_k_path_into(0)` should be the identity but destroys the
            // subtrie in pathmap 0.3.1.
            if k == 0 {
                s.emit("join_k_path_into", "skip");
            } else {
                // The bool is another `AbstractNodeRef` leak: an empty node still
                // comes back as `Some(...)` from `into_option()` for some
                // representations.  Compared only when something survived.
                let r = s.wz.join_k_path_into(&OPS, k, NO_PRUNE);
                let ret =
                    if s.wz.focus_node_is_empty() { "?".to_string() } else { show_bool(r).to_string() };
                s.emit("join_k_path_into", &ret);
            }
        }
        43 => {
            let p = d.path(6)?;
            // `insert_prefix("")` destroys the subtrie in pathmap 0.3.1.
            if p.is_empty() {
                s.emit("insert_prefix", "skip");
            } else {
                let r = s.wz.insert_prefix(&p);
                s.emit("insert_prefix", show_bool(r));
            }
        }
        44 => {
            let n = d.modn(6)?;
            let r = s.wz.remove_prefix(n);
            s.emit("remove_prefix", show_bool(r));
        }
        45 => {
            let _pr = d.boolean()?;
            let leaky = s.wz.focus_node_is_empty() && s.wz.val().is_none();
            let m = s.wz.take_map(NO_PRUNE);
            if leaky {
                let mm = m.unwrap_or_else(PathMap::empty);
                s.wz.graft_map(&mm);
                s.emit("take_map_restore", "?");
            } else {
                match m {
                    Some(mm) => {
                        s.wz.graft_map(&mm);
                        s.emit("take_map_restore", "1");
                    }
                    None => s.emit("take_map_restore", "0"),
                }
            }
        }
        46 => {
            let k = d.modn(4)?;
            let _pr = d.boolean()?;
            // `meet_k_path_into` is not implementable for these arguments; the
            // crate side applies the same guard.
            if s.wz.meet_k_path_unspecified(k) {
                s.emit("meet_k_path_into", "skip");
            } else {
                let r = s.wz.meet_k_path_into(&OPS, k, NO_PRUNE);
                s.emit("meet_k_path_into", show_bool(r));
            }
        }
        47 => {
            let t = d.modn(2)?;
            // The blind-zipper addition: `descend_until` reporting the bytes it
            // descended.  The observer's output is a blind zipper's only account
            // of where it went, so it is compared byte for byte.
            let (r, obs) = s.target(t).descend_until_observed();
            let ret = format!("{}:{}", show_bool(r), hex_path(&obs));
            s.emit("descend_until_observed", &ret);
        }
        48 => {
            let v = d.u8()?;
            // Writing through the reference `get_val_mut` hands back.  It must
            // behave like `set_val` where a value exists and do nothing —
            // crucially, *not* create the path — where one does not.
            let old = s.wz.get_val_mut_write(v as u64);
            s.emit("get_val_mut_write", &show_val(old.as_ref()));
        }
        49 => {
            let v = d.u8()?;
            let r = s.wz.get_val_or_set_mut(v as u64);
            s.emit("get_val_or_set_mut", &show_val(Some(&r)));
        }
        50 => {
            let v = d.u8()?;
            // `ran` records whether the closure was invoked.  The contract says it
            // supplies the value "if no value exists", so invoking it when a value
            // is already present is observable to any caller whose closure has a
            // side effect.
            let (r, ran) = s.wz.get_val_or_set_mut_with(v as u64);
            let ret = format!("{}:{}", show_val(Some(&r)), show_bool(ran));
            s.emit("get_val_or_set_mut_with", &ret);
        }
        51 => {
            let t = d.modn(2)?;
            let p = d.path(6)?;
            // `get_val`/`get_val_at` differ from `val`/`val_at` only in the
            // lifetime of the reference they return, so they must give the same
            // answer.  `agree` is `1` in the model by construction; a `0` from the
            // crate is the whole point of the op.
            let z = s.target_ref(t);
            let ret = format!("{}:{}:1", show_val(z.val()), show_val(z.val_at(&p)));
            s.emit("get_val_agrees", &ret);
        }
        52 => {
            // `to_next_get_val` must advance exactly as `to_next_val` does and
            // hand back the value at the new focus.
            if s.act {
                s.emit("to_next_get_val", "skip");
            } else {
                let moved = s.rz.to_next_val();
                let v = if moved { s.rz.val().copied() } else { None };
                let ret = format!("{}:{}:1", show_bool(moved), show_val(v.as_ref()));
                s.emit("to_next_get_val", &ret);
            }
        }
        53 => {
            let n = d.modn(4)?;
            let m = d.path_n(n)?;
            let ru = d.boolean()?;
            if s.act {
                s.emit("graft_masked_branches", "skip");
            } else {
                let mask = ByteMask::of_list(m);
                s.wz.graft_masked_branches(&s.rz, &mask, ru);
                let ret = format!("{}:{}", hex_path(mask.bytes()), show_bool(ru));
                s.emit("graft_masked_branches", &ret);
            }
        }
        54 => {
            let n = d.modn(4)?;
            let _m = d.path_n(n)?;
            let _ru = d.boolean()?;
            // Skipped outright: `graft_child_maps` is broken three ways
            // (FINDINGS.md #15) and the node representations it leaves behind
            // degrade the `AlgebraicStatus` that *later* operations report, which
            // would contaminate the whole run.
            s.emit("graft_child_maps", "skip");
        }
        55 => {
            let p = d.path(6)?;
            // `meet_2` takes two sources; the second is the first moved to `p`.
            if s.act {
                s.emit("meet_2", "skip");
            } else {
                let mut b = s.rz.clone();
                b.path.extend_from_slice(&p);
                let st = s.wz.meet_2(&OPS, &s.rz, &b);
                s.emit("meet_2", &show_status(st));
            }
        }
        _ => s.emit("nop", "-"),
    }
    Some(())
}

// ---------------------------------------------------------------------------
// Header and entry point — `Fuzz.header` / `Fuzz.run`
// ---------------------------------------------------------------------------

/// Decode `n` seed entries and insert them into `t`.
fn seed(t: &mut PathMap<u64>, d: &mut Dec, n: usize) -> Option<()> {
    for _ in 0..n {
        let p = d.path(6)?;
        let v = d.u8()?;
        t.set_val(&p, v as u64);
    }
    Some(())
}

/// Decode the header: two seeded maps and the two zipper roots.
fn header(d: &mut Dec, act: bool) -> Option<St> {
    let n0 = d.modn(8)?;
    let mut m0 = PathMap::empty();
    seed(&mut m0, d, n0)?;
    let n1 = d.modn(8)?;
    let mut m1 = PathMap::empty();
    seed(&mut m1, d, n1)?;
    let r0 = d.path(4)?;
    let r1 = d.path(4)?;
    // Both zipper roots are created if absent.  A zipper whose *root* does not
    // exist can escape it — `to_next_sibling_byte` and `to_next_step` fall back
    // on the parent's child mask and walk out of the granted subtrie.  Making the
    // roots exist keeps that one bug from contaminating every other comparison.
    if !r0.is_empty() {
        m0.add_path(&r0);
    }
    if !r1.is_empty() {
        m1.add_path(&r1);
    }
    Some(St {
        wz: Zip::at(m0, &r0),
        rz: Zip::at(m1, &r1),
        out: Vec::new(),
        step: 0,
        act,
    })
}

/// Decode and run a fuzzer input, returning the trace lines.
fn run(bytes: &[u8], act: bool) -> Vec<String> {
    let mut d = Dec { bytes, pos: 0 };
    let Some(mut s) = header(&mut d, act) else {
        return vec!["EMPTY".to_string()];
    };
    for _ in 0..MAX_STEPS {
        if step(&mut s, &mut d).is_none() {
            break;
        }
    }
    let mut out = s.out;
    out.push(format!("MAP0 {}", dump_at(&s.wz.trie, &[])));
    out.push(format!("MAP1 {}", dump_at(&s.rz.trie, &[])));
    out.push(format!("ROOT0 {}", hex_path(&s.wz.root)));
    out.push(format!("ROOT1 {}", hex_path(&s.rz.root)));
    out
}

// The resident-server protocol.  Plumbing only — it knows nothing about tries,
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
    for line in run(&bytes, act) {
        println!("{line}");
    }
}
