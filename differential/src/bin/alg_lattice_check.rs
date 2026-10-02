//! Is symmetric difference ambiguous, or is the value type broken?
//!
//! `(a | b) \ (a & b)` and `(a \ b) | (b \ a)` are equal in any distributive
//! lattice with a relative complement, so a divergence between them is never a
//! matter of convention.  This prints both formulas for `u64`, whose `Lattice`
//! impl is not a lattice, and for `bool`, which is a genuine two-element Boolean
//! algebra -- and shows that only the former disagrees.
//!
//! Run it when wondering whether a finding is about the crate or about the value
//! type the fuzzer happens to use:
//!
//! ```sh
//! cargo run --release -p differential --bin alg_lattice_check
//! ```

use pathmap::ring::{DistributiveLattice, Lattice, AlgebraicResult};

fn show<T: core::fmt::Debug + Clone>(r: AlgebraicResult<T>, l: &T, rr: &T) -> String {
    match r {
        AlgebraicResult::Element(v) => format!("{v:?}"),
        AlgebraicResult::Identity(m) => {
            // SELF_IDENT == 1
            if m & 1 != 0 { format!("{l:?}") } else { format!("{rr:?}") }
        }
        AlgebraicResult::None => "BOTTOM".to_string(),
    }
}

/// `(a v b) \ (a ^ b)`  vs  `(a \ b) v (b \ a)` -- equal in any distributive
/// lattice with a relative complement.
fn two_formulas<T>(a: T, b: T) -> (String, String, String, String)
where T: Lattice + DistributiveLattice + Clone + core::fmt::Debug + PartialEq
{
    let join = a.pjoin(&b);
    let meet = a.pmeet(&b);
    let j = match join.clone() { AlgebraicResult::Element(v)=>v, AlgebraicResult::Identity(m)=>if m&1!=0 {a.clone()} else {b.clone()}, AlgebraicResult::None=>a.clone() };
    let m = match meet.clone() { AlgebraicResult::Element(v)=>v, AlgebraicResult::Identity(k)=>if k&1!=0 {a.clone()} else {b.clone()}, AlgebraicResult::None=>a.clone() };
    let f1 = show(j.psubtract(&m), &j, &m);

    let ab = a.psubtract(&b);
    let ba = b.psubtract(&a);
    let f2 = match (ab, ba) {
        (AlgebraicResult::None, AlgebraicResult::None) => "BOTTOM".to_string(),
        (AlgebraicResult::None, x) => show(x, &b, &a),
        (x, AlgebraicResult::None) => show(x, &a, &b),
        (x, y) => {
            let xv = match x { AlgebraicResult::Element(v)=>v, _=>a.clone() };
            let yv = match y { AlgebraicResult::Element(v)=>v, _=>b.clone() };
            show(xv.pjoin(&yv), &xv, &yv)
        }
    };
    (show(join, &a, &b), show(meet, &a, &b), f1, f2)
}

fn main() {
    println!("u64 -- the type the fuzzer uses (marked \"GOAT trash\" in ring.rs)");
    for (a, b) in [(1u64, 2u64), (3, 7), (5, 5)] {
        let (j, m, f1, f2) = two_formulas(a, b);
        println!(
            "  a={a} b={b}:  a|b = {j:>6}   a&b = {m:>6}   (a|b)-(a&b) = {f1:>6}   (a-b)|(b-a) = {f2:>6}   {}",
            if f1 == f2 { "agree" } else { "DIVERGE" }
        );
    }
    println!("\n  Note a|b == a&b for every pair above.  In a lattice, a/\\b == a\\/b");
    println!("  implies a == b, so this impl asserts 1 == 2.  It is not a lattice:");
    println!("  pjoin is left_biased_pjoin and pmeet is Identity(SELF_IDENT), i.e.");
    println!("  both are the function \"return the left operand\".");

    println!("\nbool -- a real two-element Boolean algebra, same file");
    for (a, b) in [(true, false), (false, true), (true, true), (false, false)] {
        let (j, m, f1, f2) = two_formulas(a, b);
        println!(
            "  a={a:<5} b={b:<5}: a|b = {j:>6}  a&b = {m:>6}  (a|b)-(a&b) = {f1:>6}  (a-b)|(b-a) = {f2:>6}  {}",
            if f1 == f2 { "agree" } else { "DIVERGE" }
        );
    }
}
