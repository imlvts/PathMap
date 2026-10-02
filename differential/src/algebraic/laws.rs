//! Identities that must hold whatever the operands are.
//!
//! The route comparison in `routes.rs` catches an implementation that disagrees
//! with its siblings.  It cannot catch a mistake they all share.  Laws are the
//! other half: each one is two *different expressions* that must evaluate to
//! the same trie, so a wrong answer is visible even when every route computes
//! it the same wrong way.
//!
//! A law is written as a pair of [`Expr`]s and evaluated by the ordinary route
//! machinery, which keeps this file declarative and means a law automatically
//! inherits whatever the expression evaluator can do.
//!
//! # Why some laws are weakened here, and why that is the value type's fault
//!
//! `u64`'s lattice instances are degenerate.  `pjoin` is `left_biased_pjoin`
//! and `pmeet` is `Identity(SELF_IDENT)`: both return the left operand, so
//! `a | b == a & b` for every pair.  In a lattice `a & b == a | b` forces
//! `a == b`, so the impl asserts `1 == 2` and is not a lattice -- `pathmap::ring`
//! marks it `//GOAT trash`.
//!
//! Everything below that looks like a weakened law follows from that, and from
//! nothing deeper.  The lattice identities themselves are not in doubt:
//!
//! * The laws marked [`Level::Paths`] would be ordinary value-level laws under a
//!   real lattice.  They are weakened because with both operations returning the
//!   left operand, swapping the operands swaps the result's value, so
//!   commutativity cannot hold on values here.
//!
//! * The three identities listed below as "not laws" **are** laws in any
//!   distributive lattice.  They fail only because join and meet have collapsed
//!   into one function and `psubtract` is `None` exactly on equality.
//!
//! So this list is a record of what `u64` costs, not of anything the algebra
//! does wrong.  `bin/alg_lattice_check.rs` prints the tables, and
//! `../../ALGEBRAIC_FUZZING.md` explains what it costs in coverage -- chiefly
//! that `AlgebraicResult::Element` is unreachable from `u64`'s `pjoin` and
//! `pmeet`, so the code that stores a genuinely *combined* value never runs.
//!
//! # Identities absent for that reason
//!
//! Listed so nobody adds them back without also changing the value type:
//!
//! * `a - (b | c) == (a - b) & (a - c)`.  `b | c` carries `b`'s value where both
//!   are present, so the left side keeps a path whose value matches `c` but not
//!   `b`, while the right side drops it.
//! * `a - b == a - (a & b)`.  `a & b` carries *`a`'s* value -- because `pmeet`
//!   ignores its argument -- so the right side drops every shared path
//!   regardless of value, while the left side drops only the equal ones.
//! * `(a ^ b) ^ c == a ^ (b ^ c)` on values.  A path in all three cancels in the
//!   inner operation either way, leaving `c`'s value on the left and `a`'s on
//!   the right.  Checked on paths, where it does hold.

use super::expr::{Expr, Op};

/// Operand slot holding the empty trie, appended after the case's own
/// operands so identities can mention it.  See [`LAW_OPERANDS`].
pub const EMPTY: usize = 3;

/// Operands a law may mention: three from the case, then the empty trie.
pub const LAW_OPERANDS: usize = 4;

/// How much of the result a law constrains.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    /// Which path carries which value.
    Values,
    /// Which paths survive, values unconstrained.
    Paths,
}

pub struct Law {
    pub name: &'static str,
    pub lhs: Expr,
    pub rhs: Expr,
    pub level: Level,
}

fn v(i: usize) -> Expr {
    Expr::Var(i)
}
fn j(a: Expr, b: Expr) -> Expr {
    Expr::bin(Op::Join, a, b)
}
fn m(a: Expr, b: Expr) -> Expr {
    Expr::bin(Op::Meet, a, b)
}
fn s(a: Expr, b: Expr) -> Expr {
    Expr::bin(Op::Subtract, a, b)
}
fn x(a: Expr, b: Expr) -> Expr {
    Expr::bin(Op::SymDiff, a, b)
}
fn r(a: Expr, b: Expr) -> Expr {
    Expr::bin(Op::Restrict, a, b)
}

pub fn laws() -> Vec<Law> {
    use Level::{Paths, Values};
    let (a, b, c, e) = (0usize, 1usize, 2usize, EMPTY);
    vec![
        // --- lattice basics
        Law { name: "join-idempotent", lhs: j(v(a), v(a)), rhs: v(a), level: Values },
        Law { name: "meet-idempotent", lhs: m(v(a), v(a)), rhs: v(a), level: Values },
        Law { name: "join-unit", lhs: j(v(a), v(e)), rhs: v(a), level: Values },
        Law { name: "meet-zero", lhs: m(v(a), v(e)), rhs: v(e), level: Values },
        // Path set only: both `pjoin` and `pmeet` return the left operand, so
        // swapping the operands swaps which value lands.  A real lattice would
        // make these value-level laws; see the module comment.
        Law { name: "join-commutative", lhs: j(v(a), v(b)), rhs: j(v(b), v(a)), level: Paths },
        Law { name: "meet-commutative", lhs: m(v(a), v(b)), rhs: m(v(b), v(a)), level: Paths },
        // Associativity does hold on values even here: both nestings select the
        // leftmost present operand's value.
        Law {
            name: "join-associative",
            lhs: j(j(v(a), v(b)), v(c)),
            rhs: j(v(a), j(v(b), v(c))),
            level: Values,
        },
        Law {
            name: "meet-associative",
            lhs: m(m(v(a), v(b)), v(c)),
            rhs: m(v(a), m(v(b), v(c))),
            level: Values,
        },
        Law { name: "absorb-meet-join", lhs: m(v(a), j(v(a), v(b))), rhs: v(a), level: Values },
        Law { name: "absorb-join-meet", lhs: j(v(a), m(v(a), v(b))), rhs: v(a), level: Values },
        Law {
            name: "meet-distributes-over-join",
            lhs: m(v(a), j(v(b), v(c))),
            rhs: j(m(v(a), v(b)), m(v(a), v(c))),
            level: Values,
        },
        Law {
            name: "join-distributes-over-meet",
            lhs: j(v(a), m(v(b), v(c))),
            rhs: m(j(v(a), v(b)), j(v(a), v(c))),
            level: Values,
        },
        // --- subtraction
        Law { name: "subtract-self", lhs: s(v(a), v(a)), rhs: v(e), level: Values },
        Law { name: "subtract-unit", lhs: s(v(a), v(e)), rhs: v(a), level: Values },
        Law { name: "subtract-from-empty", lhs: s(v(e), v(a)), rhs: v(e), level: Values },
        // Each step drops the paths whose value the subtrahend matches, and
        // "matches b or matches c" does not depend on the order.
        Law {
            name: "subtract-steps-commute",
            lhs: s(s(v(a), v(b)), v(c)),
            rhs: s(s(v(a), v(c)), v(b)),
            level: Values,
        },
        // Subtracting cannot add a path: re-joining the removed part recovers
        // no more than the original.
        Law { name: "subtract-shrinks", lhs: j(s(v(a), v(b)), v(a)), rhs: v(a), level: Values },
        // --- symmetric difference
        Law { name: "sym-diff-self", lhs: x(v(a), v(a)), rhs: v(e), level: Values },
        Law { name: "sym-diff-unit", lhs: x(v(a), v(e)), rhs: v(a), level: Values },
        // Fully commutative, values included: a coincident path cancels on both
        // sides, so each surviving value comes from the single side that has it.
        Law { name: "sym-diff-commutative", lhs: x(v(a), v(b)), rhs: x(v(b), v(a)), level: Values },
        Law {
            name: "sym-diff-associative-paths",
            lhs: x(x(v(a), v(b)), v(c)),
            rhs: x(v(a), x(v(b), v(c))),
            level: Paths,
        },
        // The definition `zipper_sym_diff`'s own documentation gives.
        Law {
            name: "sym-diff-is-join-minus-meet",
            lhs: x(v(a), v(b)),
            rhs: s(j(v(a), v(b)), m(v(a), v(b))),
            level: Values,
        },
        // --- restrict
        // Every path is a prefix of itself, so restricting by its own operand
        // admits everything.
        Law { name: "restrict-self", lhs: r(v(a), v(a)), rhs: v(a), level: Values },
        Law { name: "restrict-empty", lhs: r(v(a), v(e)), rhs: v(e), level: Values },
        Law { name: "restrict-of-empty", lhs: r(v(e), v(a)), rhs: v(e), level: Values },
        Law { name: "restrict-idempotent", lhs: r(r(v(a), v(b)), v(b)), rhs: r(v(a), v(b)), level: Values },
        // A path present in both operands is admitted by itself, so the meet is
        // contained in the restriction.
        Law {
            name: "meet-under-restrict",
            lhs: m(m(v(a), v(b)), r(v(a), v(b))),
            rhs: m(v(a), v(b)),
            level: Values,
        },
        // --- majority, the worked example in `zipper_majority`'s docs, written
        // out as the DNF it claims to equal.  Exercised here so the DNF engine
        // is checked against a hand-written join of meets as well as against
        // the pointwise routes.
        Law {
            name: "majority-is-pairwise-meets",
            lhs: j(j(m(v(a), v(b)), m(v(a), v(c))), m(v(b), v(c))),
            rhs: j(m(v(a), v(b)), j(m(v(a), v(c)), m(v(b), v(c)))),
            level: Values,
        },
    ]
}
