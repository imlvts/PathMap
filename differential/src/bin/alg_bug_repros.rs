//! Standalone reproducers for what `bin/alg_fuzz` found on `master`.
//!
//! Same role as `bin/zipper_bug_repros.rs`: each case is a dozen lines of
//! ordinary `pathmap` calls with no fuzzer, no harness and no decoder, so it
//! can be pasted into a test or stepped through in a debugger.  The signatures
//! these correspond to are listed in `algebraic::KNOWN`.
//!
//! ```sh
//! cargo run --release -p differential --bin alg_bug_repros
//! ```
//!
//! Exit status is 1 while any of them still reproduces.  Case 8 is a
//! disagreement rather than a defect and never counts as a failure.

use pathmap::PathMap;
use pathmap::fuse::FuseExpr;
use pathmap::zipper::{ZipperMoving, ZipperValues, ZipperWriting};

/// A trie holding only dangling paths: structure written with `create_path`
/// and no value anywhere.
fn dangling(paths: &[&[u8]]) -> PathMap<u64> {
    let mut m = PathMap::<u64>::new();
    let mut wz = m.write_zipper();
    for p in paths {
        wz.reset();
        wz.descend_to(*p);
        wz.create_path();
    }
    drop(wz);
    m
}

fn with_val(path: &[u8], v: u64) -> PathMap<u64> {
    let mut m = PathMap::<u64>::new();
    m.write_zipper_at_path(path).set_val(v);
    m
}

fn val_at(m: &PathMap<u64>, path: &[u8]) -> Option<u64> {
    m.read_zipper_at_path(path).val().copied()
}

struct Report {
    failed: usize,
}

impl Report {
    fn case(&mut self, n: &str, what: &str, want: String, got: String) {
        let ok = want == got;
        if !ok {
            self.failed += 1;
        }
        println!("\n{n}. {what}");
        println!("   expected: {want}");
        println!("   got:      {got}");
        println!("   => {}", if ok { "fixed" } else { "REPRODUCES" });
    }
}

fn main() {
    let mut r = Report { failed: 0 };

    // ---------------------------------------------------------------- 1
    // `join_into` is the write-zipper spelling of `join`, so joining a source
    // into an empty destination has to be the same as joining the empty map
    // with it.  The root value does not survive the write-zipper form.
    {
        let src = with_val(&[], 1);
        let eager = PathMap::<u64>::new().join(&src);

        let mut out = PathMap::<u64>::new();
        {
            let mut wz = out.write_zipper();
            wz.join_into(&src.read_zipper());
        }
        r.case(
            "1",
            "join_into on an empty destination: empty | {_:1}",
            format!("root value {:?} (PathMap::join)", val_at(&eager, &[])),
            format!("root value {:?} (join_into)", val_at(&out, &[])),
        );
    }

    // ---------------------------------------------------------------- 2
    // `meet_2` writes the intersection of two sources into a fresh
    // destination.  Two maps whose only content is a root value intersect to
    // that root value; `meet_2` produces nothing at all.
    {
        let a = with_val(&[], 1);
        let b = with_val(&[], 2);
        let eager = b.meet(&a);

        let mut out = PathMap::<u64>::new();
        {
            let mut wz = out.write_zipper();
            wz.meet_2(&b.read_zipper(), &a.read_zipper());
        }
        r.case(
            "2",
            "meet_2 with root values only: {_:2} & {_:1}",
            format!("root value {:?} (PathMap::meet)", val_at(&eager, &[])),
            format!("root value {:?} (meet_2)", val_at(&out, &[])),
        );
    }

    // ---------------------------------------------------------------- 3
    // `pjoin` on `u64` is `left_biased_pjoin` and `pmeet` is
    // `Identity(SELF_IDENT)`, so where both operands have a value at a path the
    // result must carry the *left* one.  It carries the right one when the two
    // operands' nodes are laid out differently -- here because `a` has a chain
    // of dangling descendants below the shared path and `c` does not.
    //
    // Every zipper route gets this right; `PathMap::join` and `PathMap::meet`
    // do not, which is why it surfaces as failing laws rather than as one
    // route disagreeing with the rest.
    {
        let mut a = dangling(&[&[0, 0, 0, 0], &[1]]);
        a.write_zipper_at_path(&[0]).set_val(1);
        let c = with_val(&[0], 2);

        r.case(
            "3a",
            "join value bias: {00:2} | {00:1, dangling below} at [0]",
            "Some(2), the left operand's value".to_string(),
            format!("{:?}", val_at(&c.join(&a), &[0])),
        );
        r.case(
            "3b",
            "meet value bias: {00:1, dangling below} & {00:2} at [0]",
            "Some(1), the left operand's value".to_string(),
            format!("{:?}", val_at(&a.meet(&c), &[0])),
        );
    }

    // ---------------------------------------------------------------- 4
    // The worst of them: a join *loses a value*.  Not a bias about which of two
    // values wins -- only one operand has a value at all, and the result has
    // none.
    //
    // `b` is a clone of `a` with one value added, so the two tries share nodes.
    // Joining in the order `a | b` drops `b`'s value; `b | a` keeps it.  A
    // join is a least upper bound, so no ordering of the operands may lose a
    // path, which makes this the one finding here that is unambiguous without
    // any appeal to value bias.
    //
    // The sharing is essential: rebuilding `b`'s entries into a fresh map
    // instead of cloning `a` makes it go away.
    {
        let a = dangling(&[&[0, 0, 0, 0, 0], &[1, 0, 1], &[1, 0, 3]]);
        let mut b = a.clone();
        b.write_zipper_at_path(&[1, 0, 0, 0]).set_val(1);

        let ab = a.join(&b);
        let ba = b.join(&a);
        r.case(
            "4",
            "join drops a value across clone-shared structure, at [1,0,0,0]",
            format!("a|b == b|a == Some(1); b|a gives {:?}", val_at(&ba, &[1, 0, 0, 0])),
            format!("a|b gives {:?}", val_at(&ab, &[1, 0, 0, 0])),
        );
    }

    // ---------------------------------------------------------------- 5
    // Not an algebraic defect: `merkleize` panics outright on a trie that holds
    // structure but no values.  It is in this list because the fuzzer's operand
    // generator reaches it -- `merkleize` is how it builds operands with shared
    // subtries, which is what case 4 needs.
    {
        let mut m = dangling(&[&[0, 0], &[0, 1, 0, 0]]);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.merkleize()));
        std::panic::set_hook(hook);
        r.case(
            "5",
            "merkleize over dangling-only structure",
            "no panic".to_string(),
            if res.is_ok() { "no panic".to_string() } else { "panic".to_string() },
        );
    }

    // ---------------------------------------------------------------- 6
    // Finding 6 has no standalone reproducer: it is three `debug_assert!`s
    // inside the merge primitives, so there is nothing to compare and nothing
    // to print -- the assertion either fires or it does not.  Build with
    // `-C debug-assertions=yes` and replay the three `panic-eval-*` inputs in
    // `algebraic-corpus/`.  Case 7 below is its visible consequence.
    println!(
        "\n6. join reporting an empty result from non-empty nodes\n            => debug-assertions only; replay algebraic-corpus/panic-eval-*.bin"
    );

    // ---------------------------------------------------------------- 7
    // Finding 4 again, reached without cloning anything, and with a visible
    // consequence.  `fuse`'s `Xor` is `(l \ r) | (r \ l)`.  With `c` holding no
    // values at all and `a` holding one, `c \ a` comes out as dangling-only
    // structure and `a \ c` keeps the value -- so the join at the end is
    // exactly the shape of finding 4, and it loses the value.
    //
    // Worth having separately because every PathMap-level spelling of the same
    // thing keeps it: `a - c`, `(c | a) - (c & a)` and `(c - a) | (a - c)` are
    // all correct here.  Only the node-level composition loses it, and
    // `join_into_dyn` reports `AlgebraicStatus::Element` while doing so, so a
    // caller cannot detect it from the status either.
    {
        let mut a = dangling(&[&[0, 0, 0, 0, 0]]);
        a.write_zipper_at_path(&[0]).set_val(1);
        let c = dangling(&[&[1]]);

        let (prog, out) = FuseExpr::xor(FuseExpr::leaf(0), FuseExpr::leaf(1)).compile();
        let fused = prog.eval(&[&c, &a], &[out]).pop().unwrap();

        let eager = c.join(&a).subtract(&c.meet(&a));
        r.case(
            "7",
            "fuse Xor loses a value only one operand has, at [0]",
            format!("Some(1), as (c|a)-(c&a) gives {:?}", val_at(&eager, &[0])),
            format!("{:?}", val_at(&fused, &[0])),
        );
    }

    // ---------------------------------------------------------------- 8
    // Not a defect, a disagreement: two conventions for what symmetric
    // difference does to a coincident path carrying *different* values.
    //
    // `zipper_sym_diff`'s value policy cancels it -- for `u64`, `pjoin` and
    // `pmeet` are both `Identity`, so `SymDiff::combine_impl` reaches `join ==
    // meet` and yields nothing.  `(a | b) - (a & b)`, the definition its own
    // documentation gives, agrees.  `fuse`'s `Xor` is `(l \ r) | (r \ l)`,
    // which for `u64` keeps the left value, because `psubtract` is a no-op on
    // differing values.
    //
    // Classically the two definitions are equal.  They are not equal over this
    // value lattice, and nothing in the crate says which one is meant.
    {
        let c = with_val(&[], 2);
        let a = with_val(&[], 1);

        let (prog, out) = FuseExpr::xor(FuseExpr::leaf(0), FuseExpr::leaf(1)).compile();
        let fused = prog.eval(&[&c, &a], &[out]).pop().unwrap();
        let by_definition = c.join(&a).subtract(&c.meet(&a));

        println!("\n8. symmetric difference of {{_:2}} and {{_:1}}: two conventions");
        println!("   (c|a)-(c&a) root value: {:?}  (cancels)", val_at(&by_definition, &[]));
        println!("   fuse Xor    root value: {:?}  (keeps the left)", val_at(&fused, &[]));
        println!("   => not counted as a failure; the crate has not chosen");
    }

    println!("\n{} of 7 still reproduce", r.failed);
    if r.failed > 0 {
        std::process::exit(1);
    }
}
