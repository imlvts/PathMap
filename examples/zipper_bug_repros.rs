//! Minimal reproducers for the zipper-API defects the Lean differential model
//! turned up.  See `lean/FINDINGS.md` for the write-up.
//!
//!     cargo run --example zipper_bug_repros -- --list
//!     cargo run --example zipper_bug_repros -- <name>
//!     cargo run --example zipper_bug_repros -- --all       # skips the hang
//!
//! Several cases abort in a debug build (debug assertions / overflow checks) and
//! misbehave silently in release, so each one is selectable individually.

use pathmap::PathMap;
use pathmap::utils::ByteMask;
use pathmap::zipper::*;

fn vals(m: &PathMap<u64>) -> String {
    let v: Vec<String> = m.iter().map(|(k, x)| format!("{k:?}={x}")).collect();
    if v.is_empty() { "<no values>".into() } else { v.join(" ") }
}

fn paths(m: &PathMap<u64>) -> String {
    let mut rz = m.read_zipper();
    let mut v = vec![format!("_{}", mark(rz.val()))];
    while rz.to_next_step() {
        v.push(format!("{:?}{}", rz.path(), mark(rz.val())));
    }
    v.join(" ")
}

fn mark(v: Option<&u64>) -> String {
    match v { Some(x) => format!("={x}"), None => String::new() }
}

/// `{[] -> 0, [0,0,0,0] -> 7}` with dangling interior locations.
fn chain_map() -> PathMap<u64> {
    let mut m = PathMap::<u64>::new();
    { let mut w = m.write_zipper(); w.set_val(0); }
    m.insert(&[0u8, 0, 0, 0], 7);
    m
}

const CASES: &[(&str, &str)] = &[
    ("join_into_empty_dst", "join_into silently drops the source when the destination map is empty"),
    ("to_next_val_after_step", "to_next_val misses every downstream value after descend_first_byte / to_next_step / descend_first_k_path"),
    ("root_escape", "a read zipper whose root does not exist walks out of its own root"),
    ("insert_prefix_empty", "insert_prefix(b\"\") destroys the subtrie instead of doing nothing"),
    ("drop_head_zero", "join_k_path_into(0) destroys the subtrie instead of doing nothing"),
    ("prune_reach", "prune_path's depth and reported byte count depend on internal node layout"),
    ("empty_node_leak", "remove_branches / take_map report on node materialisation, not trie state"),
    ("ascend_until_wz", "ascend_until corrupts a write zipper rooted at a node boundary"),
    ("to_next_k_path_borrowed", "to_next_k_path underflows path_len on a borrowed-path zipper"),
    ("prev_sibling_missing", "to_prev_sibling_byte asserts path_exists on a non-existent focus"),
    ("remove_unmasked_dangling", "remove_unmasked_branches asserts inside a dangling line node"),
    ("meet_k_path_hang", "meet_k_path_into loops forever when the focus has no children (HANGS)"),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--list" {
        for (name, desc) in CASES {
            println!("{name:26} {desc}");
        }
        return;
    }
    if args[0] == "--all" {
        for (name, _) in CASES {
            if *name == "meet_k_path_hang" {
                println!("\n### meet_k_path_hang: skipped (it does not terminate)");
                continue;
            }
            println!("\n### {name}");
            run(name);
        }
        return;
    }
    for a in &args {
        println!("### {a}");
        run(a);
    }
}

fn run(name: &str) {
    match name {
        // `join_into` reports Identity and writes nothing when the destination
        // map is empty, even though the source subtrie is not.  `graft` from the
        // same read zipper copies it correctly, so the data is reachable; the
        // join just discards it.
        "join_into_empty_dst" => {
            let mut src = PathMap::<u64>::new();
            src.insert(&[0u8, 0, 0, 0], 7);
            let mut rz = src.read_zipper();
            rz.descend_to(&[0u8]);
            println!("  source at [0]: child_count={} val_count={} make_map={}",
                     rz.child_count(), rz.val_count(), vals(&rz.make_map()));

            let mut empty = PathMap::<u64>::new();
            let st = { let mut w = empty.write_zipper(); w.join_into(&rz) };
            println!("  into EMPTY dst    -> {st:?}; dst = {}   <-- source lost", vals(&empty));

            let mut nonempty = PathMap::<u64>::new();
            nonempty.insert(&[9u8], 5);
            let st = { let mut w = nonempty.write_zipper(); w.join_into(&rz) };
            println!("  into NONEMPTY dst -> {st:?}; dst = {}", vals(&nonempty));

            let mut grafted = PathMap::<u64>::new();
            { let mut w = grafted.write_zipper(); w.graft(&rz); }
            println!("  graft into EMPTY  -> dst = {}", vals(&grafted));
        }

        // Three movement operations leave iteration state that makes the next
        // `to_next_val` give up immediately.  Every route below lands the zipper
        // on exactly the same location; only *how it got there* differs.
        "to_next_val_after_step" => {
            let mut map = PathMap::<u64>::new();
            { let mut w = map.write_zipper(); w.set_val(0); }
            map.insert(&[0u8, 0], 7);
            println!("  trie: {}   ([0] exists but holds no value)", paths(&map));
            println!("  each route lands on [0]; to_next_val() should then reach [0,0]");

            type Rz<'a> = pathmap::zipper::ReadZipperUntracked<'a, 'static, u64>;
            let ways: Vec<(&str, Box<dyn Fn(&mut Rz)>)> = vec![
                ("descend_to([0])", Box::new(|z: &mut Rz| { z.descend_to(&[0u8]); })),
                ("descend_to_byte(0)", Box::new(|z: &mut Rz| { z.descend_to_byte(0); })),
                ("descend_indexed_byte(0)", Box::new(|z: &mut Rz| { z.descend_indexed_byte(0); })),
                ("descend_last_byte()", Box::new(|z: &mut Rz| { z.descend_last_byte(); })),
                ("descend_to_existing_byte(0)", Box::new(|z: &mut Rz| { z.descend_to_existing_byte(0); })),
                ("move_to_path([0])", Box::new(|z: &mut Rz| { z.move_to_path(&[0u8]); })),
                ("descend_first_byte()", Box::new(|z: &mut Rz| { z.descend_first_byte(); })),
                ("to_next_step()", Box::new(|z: &mut Rz| { z.to_next_step(); })),
                ("descend_first_k_path(1)", Box::new(|z: &mut Rz| { z.descend_first_k_path(1); })),
            ];
            for (label, f) in &ways {
                let mut z = map.read_zipper();
                f(&mut z);
                assert_eq!(z.path(), &[0u8], "{label} did not land on [0]");
                let r = z.to_next_val();
                println!("    {label:28} -> to_next_val = {r:<5} at {:?}{}",
                         z.path(), if r { "" } else { "   <-- BROKEN" });
            }
            println!("  note descend_first_byte() is documented as having \"identical behavior\"");
            println!("  to descend_indexed_byte(0), yet only the former breaks the iteration.");
            println!("  to_next_get_val() fails identically, since it delegates to to_next_val().");
        }

        // `path()` and `at_root()` still say "at my root" while `origin_path()`
        // has moved outside the subtrie the zipper was granted.  A ZipperHead
        // hands out zippers on the assumption that this cannot happen.
        "root_escape" => {
            let mut map = PathMap::<u64>::new();
            map.insert(&[0u8, 0, 3], 161);
            let mut rz = map.read_zipper_at_path(&[0u8, 0, 1]);
            println!("  root=[0,0,1] exists={} origin={:?}", rz.path_exists(), rz.origin_path());
            let moved = rz.to_next_sibling_byte();
            println!("  to_next_sibling_byte->{moved:?} at_root={} path={:?} origin={:?} val={:?}",
                     rz.at_root(), rz.path(), rz.origin_path(), rz.val());
            println!("  <-- the zipper is reading [0,0,3], outside its own root");
        }

        // Documented as the inverse of `drop_head`; with an empty prefix it must
        // be the identity.
        "insert_prefix_empty" => {
            let mut map = PathMap::<u64>::new();
            map.insert(b"ab", 1);
            println!("  before: {}", vals(&map));
            let r = { let mut w = map.write_zipper(); w.insert_prefix(b"") };
            println!("  insert_prefix(b\"\")->{r}; after: {}   <-- expected unchanged", vals(&map));
        }

        // Dropping zero leading bytes must be the identity.
        "drop_head_zero" => {
            let mut map = PathMap::<u64>::new();
            { let mut w = map.write_zipper(); w.set_val(0); }
            for k in [&[0u8][..], &[0, 0], &[1, 0]] { map.insert(k, 0u64); }
            println!("  before: {}", vals(&map));
            let r = { let mut w = map.write_zipper(); w.join_k_path_into(0, false) };
            println!("  join_k_path_into(0)->{r}; after: {}   <-- expected unchanged", vals(&map));
        }

        // `prune_path` is documented not to prune above the zipper's root, and to
        // return the number of bytes removed.  It does prune above the root, and
        // the count it returns switches between absolute and relative depending
        // on where the internal node holding the focus begins.
        "prune_reach" => {
            for len in [8usize, 100] {
                for rootlen in [0usize, 5] {
                    let key: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
                    let mut map = PathMap::<u64>::new();
                    map.insert(&key, 1);
                    let (root, rest) = key.split_at(rootlen);
                    let mut wz = map.write_zipper_at_path(root);
                    wz.descend_to(rest);
                    wz.remove_val(false);
                    let n = wz.prune_path();
                    drop(wz);
                    println!("  chain len={len:3} zipper root depth={rootlen}: prune_path->{n:3} \
                              (absolute={len}, relative={}), map now empty={}",
                             len - rootlen, map.read_zipper().child_count() == 0);
                }
            }
        }

        // Whether an *empty node* happens to be materialised at the focus is
        // representation state, not trie state -- but several return values
        // report on it.
        "empty_node_leak" => {
            let mut map = PathMap::<u64>::new();
            let mut wz = map.write_zipper();
            wz.descend_to(&[0u8]);
            println!("  create_path->{}", wz.create_path());
            println!("  focus: exists={} child_count={} val={:?}",
                     wz.path_exists(), wz.child_count(), wz.val());
            println!("  remove_branches(false)->{}   <-- nothing was removed", wz.remove_branches(false));
            match wz.take_map(false) {
                Some(m) => println!("  take_map->Some(is_empty={})   <-- expected None", m.is_empty()),
                None => println!("  take_map->None"),
            }
        }

        // Panics in debug; in release the write zipper's node stack is left
        // inconsistent and later reads through it are wrong.
        "ascend_until_wz" => {
            let mut src = PathMap::<u64>::new();
            { let mut w = src.write_zipper(); w.set_val(10); }
            src.insert(&[0u8, 0, 0, 0], 11);
            let mut map = PathMap::<u64>::new();
            { let mut w = map.write_zipper_at_path(&[0u8]); w.graft(&src.read_zipper()); }
            println!("  trie: {}", paths(&map));
            let mut wz = map.write_zipper_at_path(&[0u8]);
            println!("  at root: val_count={}", wz.val_count());
            println!("  descend_last_byte->{:?}", wz.descend_last_byte());
            println!("  ascend_until->{}  val_count={} (expected 2)", wz.ascend_until(), wz.val_count());
        }

        // `path_len()` computes `prefix_buf.len() - origin_path.len()` before the
        // path buffer exists.  Debug: overflow panic.  Release: wraps to a huge
        // usize and takes the wrong branch.
        "to_next_k_path_borrowed" => {
            let mut map = PathMap::<u64>::new();
            map.insert(b"ab", 1u64);
            let mut rz = map.read_zipper_at_borrowed_path(b"ab");
            println!("  to_next_k_path(1) on a borrowed-path zipper -> {}", rz.to_next_k_path(1));
        }

        // The default implementation's `debug_assert!(self.path_exists())` fires
        // when the focus never existed to begin with.
        "prev_sibling_missing" => {
            let mut map = PathMap::<u64>::new();
            let mut wz = map.write_zipper();
            wz.descend_to_byte(0);
            println!("  focus exists={}", wz.path_exists());
            println!("  to_prev_sibling_byte -> {:?}", wz.to_prev_sibling_byte());
        }

        "remove_unmasked_dangling" => {
            let mut map = PathMap::<u64>::new();
            map.create_path(&[0u8]);
            let mut wz = map.write_zipper_at_path(&[0u8]);
            println!("  focus exists={} child_count={}", wz.path_exists(), wz.child_count());
            wz.remove_unmasked_branches(ByteMask::EMPTY, false);
            println!("  remove_unmasked_branches(EMPTY) returned");
        }

        // Does not terminate: `descend_first_k_path`'s default loop cannot make
        // progress when the focus has no children.
        "meet_k_path_hang" => {
            let mut map = PathMap::<u64>::new();
            map.insert(b"ab", 1u64);
            let mut wz = map.write_zipper();
            wz.descend_to(b"ab");
            println!("  focus child_count={} -- calling meet_k_path_into(1, false), which hangs",
                     wz.child_count());
            let r = wz.meet_k_path_into(1, false);
            println!("  returned {r} (unreachable)");
        }

        other => println!("  unknown case {other:?}; try --list"),
    }
}
