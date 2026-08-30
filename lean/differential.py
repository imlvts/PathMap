#!/usr/bin/env python3
"""Differential runner: two implementations of the same specification.

For each input file, runs the Lean oracle and one other front end, and diffs the
traces.  All of them decode the same bytes with the same rules; see
`lean/PathMapModel/Fuzz.lean` for the wire format.

    lean/.lake/build/bin/pathmap-oracle     the Lean model  (always)
    target/*/examples/pathmap_trace        the real crate  (default)
    target/*/examples/reference            the Rust model  (--model)
    target/*/examples/act_trace            ACT read source (--act)

Each child is spawned once with `--server` and stays resident, taking inputs as
`run-input <timeout-ms> <hex>` on stdin; see `examples/common/server.rs` for the
protocol.  That replaced a temp file and two fresh processes per input, which
cost about 5x the runtime and left `/tmp/pathmap-diff-*` behind forever.  Only a
*failing* input is written to disk now, so it can still be replayed and shrunk.

    ./lean/differential.py corpus/*                # check a corpus
    ./lean/differential.py --random 500            # generate and check
    ./lean/differential.py --model --random 500    # check the Rust port

`--model` is the acceptance test for `examples/reference/`: it compares two
independent transcriptions of the same specification, in different languages,
with the crate not involved at all.  So the KNOWN table below does not apply —
every divergence is a bug in one of the two models, and none may be tolerated.
"""
import argparse
import os
import re
import random
import threading
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
MODEL_CANDIDATES = [
    os.path.join(ROOT, "target", "release", "examples", "reference"),
    os.path.join(ROOT, "target", "debug", "examples", "reference"),
]
TIMEOUT = 30


def find_trace_bin(act, model=False):
    if model:
        candidates = MODEL_CANDIDATES
    elif act:
        candidates = ACT_CANDIDATES
    else:
        candidates = TRACE_CANDIDATES
    for c in candidates:
        if os.path.exists(c):
            return c
    if model:
        sys.exit("build the Rust model first: "
                 "cargo build --release --example reference")
    if act:
        sys.exit("build the ACT side first: "
                 "cargo build --release --features arena_compact --example act_trace")
    sys.exit("build the crate side first: cargo build --release --example pathmap_trace")


class Child:
    """A resident trace binary, fed one input at a time over stdin.

    Spawning a process per input dominated the old runtime — 200 inputs spent
    more time in `sys` (fork/exec) than in `user` — so each front end now stays
    up and takes work as `run-input <timeout-ms> <hex>`, replying with the trace
    and one `!`-prefixed terminator.  See `examples/common/server.rs`.

    Two failure modes have to be survivable, because a wedged or dead child must
    not take the run with it:

    * the child stops answering (the Rust front ends cut their own work off with
      a thread timeout, but the oracle cannot -- see lean/Main.lean);
    * the child dies outright (an abort or a stack overflow unwinding cannot
      catch), which shows up as EOF.

    Either way the child is killed and respawned, and the input is reported as a
    timeout or a crash rather than silently skewing the comparison.
    """

    def __init__(self, argv, label):
        self.argv = argv
        self.label = label
        self.proc = None
        self.restarts = 0
        self.spawn()

    def spawn(self):
        self.proc = subprocess.Popen(
            [*self.argv, "--server"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, errors="replace", bufsize=1,
        )

    def restart(self):
        self.kill()
        self.restarts += 1
        self.spawn()

    def kill(self):
        if self.proc is None:
            return
        try:
            self.proc.kill()
            self.proc.wait(timeout=5)
        except Exception:
            pass
        self.proc = None

    def quit(self):
        if self.proc is None:
            return
        try:
            self.proc.stdin.write("quit\n")
            self.proc.stdin.flush()
            self.proc.wait(timeout=5)
        except Exception:
            self.kill()
        self.proc = None

    def run(self, blob):
        """Run one input.  Returns (lines, error) exactly as the old `run` did."""
        try:
            self.proc.stdin.write("run-input %d %s\n" % (int(TIMEOUT * 1000), blob.hex()))
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            self.restart()
            return None, "died (broken pipe)"

        # A watchdog thread rather than `select` on the pipe: `readline` on a
        # text stream pulls a whole chunk into Python's own buffer, so `select`
        # on the fd then reports "not ready" while complete lines are sitting in
        # memory, and the read stalls forever.  Killing the child instead turns a
        # hang into an EOF, which the read loop below already has to handle.
        fired = []
        watchdog = threading.Timer(TIMEOUT + 5, lambda: (fired.append(True), self.proc.kill()))
        watchdog.start()
        try:
            lines = []
            while True:
                line = self.proc.stdout.readline()
                if line == "":                   # EOF: killed by the watchdog, or died
                    self.restart()
                    return None, "TIMEOUT" if fired else "died (no output)"
                line = line.rstrip("\n")
                if line.startswith("!"):
                    if line == "!DONE":
                        return lines, None
                    if line == "!TIMEOUT":
                        return None, "TIMEOUT"
                    if line.startswith("!PANIC "):
                        return None, "panic: %s" % line[len("!PANIC "):]
                    return None, "protocol: %s" % line
                lines.append(line)
        finally:
            watchdog.cancel()


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
    (["sibling_byte"],
     "sibling movement fails after a token-maintaining move "
     "[sibling_after_iteration]"),
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
    (["src/dense_byte_node.rs", "index out of bounds"],
     "graft_child_maps() reaches node_get_child_mut with an empty key whenever "
     "the destination focus is a dense node [graft_child_maps_dense]"),
    # graft_child_maps is quarantined by op name: it is broken in two documented
    # ways (finding 15), and its surviving non-panicking behaviour on line-list
    # nodes diverges constantly.  Anything new in this op hides behind this
    # entry, so re-check it once the method is fixed.
    (["graft_child_maps"],
     "graft_child_maps() misbehaves (finding 15) [graft_child_maps_dense]"),
    (["join_map_into"],
     "AlgebraicStatus::Identity is not returned reliably when nothing changed "
     "(finding 8); reproducer in lean/corpus/"),
    (["restrict"],
     "AlgebraicStatus::Identity is not returned reliably when nothing changed "
     "(finding 8); reproducer in lean/corpus/"),
    (["graft_masked_branches"],
     "graft_masked_branches() creates the focus when grafting absent branches "
     "(finding 15) [graft_child_maps_dense]"),
    (["ambiguous path violation"],
     "graft() builds a node with both a child and a value on the same key when "
     "the destination node holds a single line [graft_ambiguous_node]"),
    (["src/trie_node.rs", "make_unique"],
     "copy-on-write cannot make a shared dangling path unique (finding 16) "
     "[shared_dangling_cow]"),
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


def compare(blob, oracle, other, other_label, act=False):
    lean, err = oracle.run(blob)
    if err:
        return "oracle %s" % err
    real, err = other.run(blob)
    if err:
        return "%s %s" % (other_label, err)
    for i, (a, b) in enumerate(zip(lean, real)):
        if a != b:
            tag = "ACT-VALCOUNT-ONLY " if act and act_valcount_only(a, b) else ""
            return "%sline %d\n  lean:  %s\n  %-5s: %s" % (tag, i, a, other_label, b)
    if len(lean) != len(real):
        return "length %d (lean) vs %d (%s)" % (len(lean), len(real), other_label)
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
    ap.add_argument("--model", action="store_true",
                    help="compare the Lean model against the Rust model "
                         "(examples/reference/) instead of against the crate")
    ap.add_argument("--max-fails", type=int, default=10,
                    help="stop after this many new divergences (0 = never stop)")
    args = ap.parse_args()

    trace_bin = find_trace_bin(args.act, args.model)
    other_label = "rust" if args.model else "crate"

    # Inputs are (name, bytes).  Nothing is written to disk: the children take
    # their input as hex on stdin, so the old directory-of-temp-files is gone
    # (it was never cleaned up either -- 98M of `/tmp/pathmap-diff-*` had
    # accumulated).  A *failing* input is written out lazily, below, so it can
    # still be replayed and shrunk.
    inputs = [(p, open(p, "rb").read()) for p in args.files]
    if args.random:
        rng = random.Random(args.seed)
        for i in range(args.random):
            n = rng.randrange(8, args.maxlen)
            inputs.append(("random#%05d" % i, bytes(rng.randrange(256) for _ in range(n))))

    oracle = Child([ORACLE] + (["--act"] if args.act else []), "oracle")
    other = Child([trace_bin] + (["--act"] if (args.act and args.model) else []), other_label)
    faildir = []          # created on first failure only

    def save(name, blob):
        if not faildir:
            faildir.append(tempfile.mkdtemp(prefix="pathmap-diff-"))
        safe = re.sub(r"[^A-Za-z0-9_.#-]", "_", os.path.basename(name))
        path = os.path.join(faildir[0], safe + ".bin")
        with open(path, "wb") as f:
            f.write(blob)
        return path

    fails = 0
    known = {}
    try:
        for name, blob in inputs:
            msg = compare(blob, oracle, other, other_label, args.act)
            if msg:
                # Model against model: the KNOWN table is a list of *crate*
                # defects, and the crate is not involved.  Every divergence is new.
                note = None if args.model else classify(msg)
                if note:
                    known[note] = known.get(note, 0) + 1
                    if args.verbose:
                        print("known %s: %s" % (name, note))
                    continue
                fails += 1
                print("FAIL %s [saved %s]: %s" % (name, save(name, blob), msg))
                if args.max_fails and fails >= args.max_fails:
                    print("... stopping after %d new divergences" % fails)
                    break
            elif args.verbose:
                print("ok   %s" % name)
    finally:
        oracle.quit()
        other.quit()

    restarts = oracle.restarts + other.restarts
    if restarts:
        print("(%d child restart(s) after a timeout or crash)" % restarts)
    hit = sum(known.values())
    print("%d/%d inputs agree (%d hit known bugs, %d new divergences)"
          % (len(inputs) - fails - hit, len(inputs), hit, fails))
    for note, n in sorted(known.items()):
        print("  known x%d: %s" % (n, note))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
