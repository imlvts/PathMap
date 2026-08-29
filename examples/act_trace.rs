//! Differential trace producer with an `ArenaCompactTree` as the read source.
//!
//!     act_trace <input-file>          # or read the bytes from stdin
//!     act_trace --check <input-file>  # also assert the structural invariants
//!
//! Same wire format, same operation table and same trace format as
//! `pathmap_trace`; the only difference is that the read side is an ACT built
//! from map1 rather than the `PathMap` itself.  The Lean model produces the
//! matching trace with `pathmap-oracle --act`, which skips the operations ACT
//! cannot serve.
//!
//! What this exercises:
//!
//!   * `ArenaCompactTree::from_zipper` -- the final `MAP1` line is dumped from
//!     the ACT, so the round trip through the arena format is compared against
//!     the model's view of the source trie, dangling paths and root value
//!     included.
//!   * every read, movement and iteration operation on `ACTZipper`, against the
//!     same specification the `PathMap` read zipper is held to.
//!
//! What it cannot exercise: ACT is read-only and, more restrictively, is not a
//! `ZipperInfallibleSubtries`, so it cannot be the *source* of a graft or an
//! algebraic merge.  Those operations report `skip`.  ACT's own merge path is
//! `ArenaCompactTree::merge_zipper_into_file`, which `act_merge_check` covers.

use pathmap::PathMap;
use pathmap::arena_compact::{ACTZipper, ArenaCompactTree};
use pathmap::ring::AlgebraicStatus;
use pathmap::utils::ByteMask;
use pathmap::zipper::*;

include!("common/harness.rs");

impl<'t, S: AsRef<[u8]>> ReadSource for ACTZipper<'t, S, u64> {
    fn dump_fork(&self) -> String {
        dump(&mut self.fork_read_zipper())
    }
    /// ACT implements `ZipperSubtries` only for `Value = ()`, and
    /// `ZipperInfallibleSubtries` not at all, so a subtrie cannot be
    /// materialised from a `u64` ACT zipper.
    fn make_map_val_count(&self) -> Option<usize> {
        None
    }
    // The merge operations keep the trait defaults: `None` / `false`, rendered
    // as `skip`.  See the `ReadSource` docs for why ACT cannot serve them.
}

fn run_act(bytes: &[u8], check: bool) -> Vec<String> {
    let mut d = Dec { bytes, pos: 0 };
    let (mut map0, map1, root0, root1) = match decode_header(&mut d) {
        Some(x) => x,
        None => return vec!["EMPTY".to_string()],
    };
    let act = ArenaCompactTree::from_zipper(map1.read_zipper(), |&v| v);
    let mut out: Vec<String> = Vec::new();
    {
        let mut rz = act.read_zipper_at_path_u64(&root1);
        run_ops(&mut d, &mut out, &mut map0, &root0, &mut rz, &root1, check);
    }
    out.push(format!("MAP0 {}", dump(&mut map0.read_zipper())));
    // Dumped from the ACT, not from map1, so `from_zipper` is under test too.
    out.push(format!("MAP1 {}", dump(&mut act.read_zipper_u64())));
    out.push(format!("ROOT0 {}", hex_path(&root0)));
    out.push(format!("ROOT1 {}", hex_path(&root1)));
    out
}

fn main() {
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
    for line in run_act(&bytes, check) {
        println!("{line}");
    }
}
