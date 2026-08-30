#!/usr/bin/env python3
"""Differential runner: Lean model vs. the real `pathmap` crate.

For each input file, runs

    lean/.lake/build/bin/pathmap-oracle <file>     (the model)
    target/*/examples/pathmap_trace <file>         (the crate)

and diffs the traces.  Both decode the same bytes with the same rules; see
`lean/PathMapModel/Fuzz.lean` for the wire format.

    ./lean/differential.py corpus/*                # check a corpus
    ./lean/differential.py --random 500            # generate and check
"""
import argparse
import os
import re
import random
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ORACLE = os.path.join(ROOT, "lean", ".lake", "build", "bin", "pathmap-oracle")
TRACE_CANDIDATES = [
    os.path.join(ROOT, "target", "release", "examples", "pathmap_trace"),
    os.path.join(ROOT, "target", "debug", "examples", "pathmap_trace"),
]
ACT_CANDIDATES = [
    os.path.join(ROOT, "target", "release", "examples", "act_trace"),
    os.path.join(ROOT, "target", "debug", "examples", "act_trace"),
]
TIMEOUT = 30


def find_trace_bin(act):
    for c in (ACT_CANDIDATES if act else TRACE_CANDIDATES):
        if os.path.exists(c):
            return c
    if act:
        sys.exit("build the ACT side first: "
                 "cargo build --release --features arena_compact --example act_trace")
    sys.exit("build the crate side first: cargo build --release --example pathmap_trace")


def run(binary, path, extra=()):
    try:
        p = subprocess.run([binary, *extra, path], capture_output=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        return None, "TIMEOUT"
    if p.returncode != 0:
        return None, "exit %d: %s" % (p.returncode, p.stderr.decode(errors="replace")[-800:])
    return p.stdout.decode(errors="replace").splitlines(), None


# Divergences already traced to confirmed `pathmap` bugs.  Keyed by the name of
# the first operation whose trace line differs (or by the panic location), so a
# run can separate "known" from "new" without hiding either.  See
# lean/README.md for the full write-up of each.
# Divergences already traced to confirmed `pathmap` defects.  Each entry is a
# list of substrings that must all appear in the report, so the table survives
# the line-number shifts between branches.  Semantic entries key on the name of
# the first operation whose trace line differs; panic entries key on the file
# and message.  See lean/FINDINGS.md for the write-up of each, and
# `cargo run --example zipper_bug_repros -- <case>` for a reproducer.
KNOWN = [
    (["ESCAPED-ROOT"],
     "a zipper left its own root: root_prefix_path() changed [root_escape]"),
    (["to_next_val"],
     "to_next_val() misses downstream values after a token-maintaining move "
     "[to_next_val_after_step]"),
    (["to_next_get_val"],
     "to_next_get_val() inherits the to_next_val() iteration defect "
     "[to_next_val_after_step]"),
    (["join_into"],
     "join_into() drops the source subtrie when the destination map is empty "
     "[join_into_empty_dst]"),
    (["ascend_until"],
     "ascend_until()/ascend_until_branch() corrupt a write zipper rooted at a "
     "node boundary [ascend_until_wz]"),
    (["remove_branches"],
     "remove_branches() returns true at a dangling tip where nothing was "
     "removed [empty_node_leak]"),
    (["take_map_restore"],
     "take_map() returns Some(empty map) at a dangling tip [empty_node_leak]"),
    # Panics.  These only abort in a debug build; differential.py prefers the
    # release binary, so they normally surface as wrong values instead.
    (["src/zipper.rs", "subtract with overflow"],
     "path_len()/excess_key_len() underflow on a zipper whose path buffer is "
     "not yet prepared [to_next_k_path_borrowed]"),
    (["src/zipper.rs", "assertion failed: self.path_exists()"],
     "to_prev_sibling_byte() asserts path_exists() at a non-existent focus "
     "[prev_sibling_missing]"),
    (["src/write_zipper.rs", "assertion `left == right`"],
     "ascend_until() leaves the write zipper's node stack inconsistent "
     "[ascend_until_wz]"),
    (["src/write_zipper.rs", "Option::unwrap()"],
     "set_val unwraps a None root value"),
    (["src/line_list_node.rs", "is_child_ptr"],
     "remove_unmasked_branches() asserts inside a dangling line node "
     "[remove_unmasked_dangling]"),
    (["src/trie_ref.rs", "out of range"],
     "TrieRef slice range underflows to a huge usize"),
    (["src/trie_node.rs", "make_unique"],
     "make_unique on an empty sentinel node"),
    # ArenaCompactTree read source (differential.py --act).
    (["ACT-VALCOUNT-ONLY"],
     "ACTZipper::val_count() counts from the zipper root, not the focus "
     "[act: val_count_ignores_focus]"),
    (["k_path_walk"],
     "ACTZipper::descend_first_k_path() only walks the leftmost chain "
     "[act: first_k_path_no_backtrack]"),
    (["descend_first_k_path"],
     "ACTZipper::descend_first_k_path() only walks the leftmost chain "
     "[act: first_k_path_no_backtrack]"),
    (["descend_last_path"],
     "ACTZipper::descend_last_path() runs one byte past the end of the trie "
     "[act: last_path_overshoots]"),
]


def act_valcount_only(a, b):
    """Do these two trace lines differ *only* in the read zipper's val_count?

    ACT's val_count is wrong at every focus below the root, so it taints nearly
    every line and would otherwise make the first-differing-op key meaningless.
    """
    pa, pb = a.split(" R=", 1), b.split(" R=", 1)
    if len(pa) != 2 or len(pb) != 2 or pa[0] != pb[0]:
        return False
    strip = lambda t: re.sub(r" n\d+", " n?", t)
    return strip(pa[1]) == strip(pb[1])


def classify(msg):
    for keys, note in KNOWN:
        if all(k in msg for k in keys):
            return note
    return None


def compare(path, trace_bin, verbose, act=False):
    model, err = run(ORACLE, path, ("--act",) if act else ())
    if err:
        return "oracle %s" % err
    real, err = run(trace_bin, path)
    if err:
        return "crate %s" % err
    for i, (a, b) in enumerate(zip(model, real)):
        if a != b:
            tag = "ACT-VALCOUNT-ONLY " if act and act_valcount_only(a, b) else ""
            return "%sline %d\n  model: %s\n  crate: %s" % (tag, i, a, b)
    if len(model) != len(real):
        return "length %d (model) vs %d (crate)" % (len(model), len(real))
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("files", nargs="*")
    ap.add_argument("--random", type=int, default=0, help="generate N random inputs")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--maxlen", type=int, default=300)
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("--act", action="store_true",
                    help="use an ArenaCompactTree as the read source")
    ap.add_argument("--max-fails", type=int, default=10,
                    help="stop after this many new divergences (0 = never stop)")
    args = ap.parse_args()

    trace_bin = find_trace_bin(args.act)
    files = list(args.files)
    tmpdir = None
    if args.random:
        rng = random.Random(args.seed)
        tmpdir = tempfile.mkdtemp(prefix="pathmap-diff-")
        for i in range(args.random):
            n = rng.randrange(8, args.maxlen)
            blob = bytes(rng.randrange(256) for _ in range(n))
            p = os.path.join(tmpdir, "in%05d.bin" % i)
            with open(p, "wb") as f:
                f.write(blob)
            files.append(p)

    fails = 0
    known = {}
    for path in files:
        msg = compare(path, trace_bin, args.verbose, args.act)
        if msg:
            note = classify(msg)
            if note:
                known[note] = known.get(note, 0) + 1
                if args.verbose:
                    print("known %s: %s" % (path, note))
                continue
            fails += 1
            print("FAIL %s: %s" % (path, msg))
            if args.max_fails and fails >= args.max_fails:
                print("... stopping after %d new divergences" % fails)
                break
        elif args.verbose:
            print("ok   %s" % path)
    hit = sum(known.values())
    print("%d/%d inputs agree (%d hit known bugs, %d new divergences)"
          % (len(files) - fails - hit, len(files), hit, fails))
    for note, n in sorted(known.items()):
        print("  known x%d: %s" % (n, note))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
