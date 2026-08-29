# Findings

Defects in `pathmap` 0.3.1 (commit `2192ebc`, default features) turned up while
writing the Lean model in this directory and running it as a differential oracle.

Every entry has a standalone reproducer:

```bash
cargo run --example zipper_bug_repros -- --list
cargo run --example zipper_bug_repros -- <case>
```

Several only *abort* in a debug build (debug assertions, overflow checks) but
misbehave silently in release; that is noted per entry.

---

## 1. `join_into` silently drops the source when the destination map is empty

`case: join_into_empty_dst` — **data loss, no diagnostic**

```rust
let mut src = PathMap::<u64>::new();
src.insert(&[0, 0, 0, 0], 7);
let mut rz = src.read_zipper();
rz.descend_to(&[0]);                       // child_count 1, val_count 1

let mut dst = PathMap::<u64>::new();       // empty
dst.write_zipper().join_into(&rz);         // -> Identity, dst stays empty
```

The same read zipper grafts correctly (`graft` produces `[0,0,0] = 7`), and the
same `join_into` into a *non-empty* destination produces the right answer.  Only
the empty-destination case loses the data, and it reports `Identity` — "self was
not modified" — rather than failing.

## 2. `to_next_val` misses every downstream value after `to_next_step`

`case: to_next_val_after_step` — **silent wrong answer**

```rust
// trie: [] = 0, [0,0,0,0] = 7   (with dangling interior locations)
let mut rz = map.read_zipper();
rz.to_next_step();                  // -> true, now at [0]
rz.to_next_val();                   // -> FALSE, resets to the root
```

Reaching the very same location with `descend_to(&[0])` instead makes
`to_next_val` return `true` at `[0,0,0,0]`.  Iteration state left behind by
`to_next_step` leaks into the next `to_next_val`; two public read-only methods
are not composable.  Anyone enumerating values from a sub-position silently sees
an empty result.

## 3. A read zipper can escape its own root

`case: root_escape` — **containment violation; relevant to `ZipperHead` safety**

```rust
map.insert(&[0, 0, 3], 161);
let mut rz = map.read_zipper_at_path(&[0, 0, 1]);   // root does not exist
rz.to_next_sibling_byte();          // -> true
// at_root() == true, path() == [], but origin_path() == [0,0,3], val() == Some(161)
```

`ZipperMoving::to_next_sibling_byte` is documented to return `false` at the root
(there is no last byte), and the `ZipperMoving` default implementation does.  The
native `ReadZipper` instead consults the last byte of the *absolute* origin path,
walks up past its own root, and lands on a sibling.  `to_next_step` reaches the
same escape through `to_next_sibling_byte`.

This is the invariant `ZipperHead` depends on to hand out non-overlapping
zippers: an escaped read zipper can observe a region another zipper is
concurrently writing.  The libFuzzer target checks exactly this
(`root_prefix_path()` must never change) and hits it on roughly 1% of random
inputs.

The write zipper does not escape in the cases tested.

## 4. `insert_prefix("")` destroys the subtrie

`case: insert_prefix_empty` — **data loss**

```rust
map.insert(b"ab", 1);
map.write_zipper().insert_prefix(b"");   // -> true; map is now { [] = 1 }
```

Prefixing every downstream path with zero bytes must be the identity;
`make_parents_in(b"", node)` discards the node instead, and `true` is still
returned.

## 5. `join_k_path_into(0)` destroys the subtrie

`case: drop_head_zero` — **data loss**

```rust
// { [] = 0, [0] = 0, [0,0] = 0, [1,0] = 0 }
map.write_zipper().join_k_path_into(0, false);   // -> true
// { [] = 0, [0] = 0 }
```

Dropping zero leading bytes must be the identity.

## 6. `meet_k_path_into` never terminates when the focus has no children

`case: meet_k_path_hang` — **hang** (the reproducer is excluded from `--all`)

```rust
map.insert(b"ab", 1);
let mut wz = map.write_zipper();
wz.descend_to(b"ab");               // a leaf: child_count == 0
wz.meet_k_path_into(1, false);      // never returns
```

`meet_k_path_into` drives `descend_first_k_path` through the loop shape shared by
`ZipperIteration`'s default implementation and `WriteZipperCore::k_path_internal`.
When the focus has no children, `descend_first_byte` fails, `to_next_sibling_byte`
fails, and the ascent loop's guard `path().len() > base_idx` is already false, so
the outer `loop` spins with no state change.

`k = 0` is broken differently: `to_next_sibling_byte` succeeds and moves the
zipper *out of* the focus's subtree, after which the operation takes and meets
subtries that do not belong to it.

The native `ReadZipper` overrides these with a token-based iterator that does
terminate, so the hang is reachable through `meet_k_path_into` and through any
zipper type that inherits the default `ZipperIteration` implementation.

## 7. `prune_path` prunes above the zipper's root, and its return value is
not a function of the trie

`case: prune_reach` — **documentation is wrong; return value is unspecifiable**

`ZipperWriting::prune_path` says "This method cannot prune the trie above the
zipper's root."  It can and does:

```rust
map.insert(b"abcd", 1);
let mut wz = map.write_zipper_at_path(b"ab");   // root at depth 2
wz.descend_to(b"cd");
wz.remove_val(false);
wz.prune_path();                                // -> 4; the map is now empty
```

Worse, the number returned is `max(node_pruned_bytes, trie_pruned_bytes)`, and
`node_pruned_bytes` depends on where the internal node holding the focus begins:

| dangling chain | zipper root depth | returned | absolute | relative |
| --- | --- | --- | --- | --- |
| 8 bytes | 0 | 8 | 8 | 8 |
| 8 bytes | 5 | 8 | 8 | 3 |
| 100 bytes | 0 | 100 | 100 | 100 |
| 100 bytes | 5 | **95** | 100 | 95 |

The *effect* is the same in every row (the map ends up empty); only the reported
count changes, switching between absolute and relative purely on node layout.
The reach of the effect is layout-dependent too, in other shapes — a
`remove_val(true)` under a zipper rooted at depth 3 was observed leaving one byte
of a dangling chain behind where the same operation at the map root removes it.

The `prune` **flag** on the other operations is worse than the explicit call.
It is handed straight to `node_remove_val` / `node_remove_all_branches`, which
prune within the node whether or not they found anything to remove:

```rust
// map = { [0] = 0 }, plus a dangling [1]
let mut wz = map.write_zipper();
wz.descend_to(&[1]);
wz.remove_val(true);      // -> None, and yet [1] is gone
```

So `remove_val(true)` can delete a location while reporting that it removed
nothing, and whether it does depends on where the node boundary falls.

Consequence for anyone specifying this API: pruning is only well-defined for the
explicit `prune_path` on a zipper rooted at the map root.  The harness passes
`prune = false` everywhere else.

## 8. Several return values report on node materialisation, not on trie state

`case: empty_node_leak` — **unspecifiable return values**

```rust
let mut wz = map.write_zipper();
wz.descend_to(&[0]);
wz.create_path();                    // -> true, a dangling tip
wz.remove_branches(false);           // -> TRUE, but nothing was removed
wz.take_map(false);                  // -> Some(map) with is_empty() == true
```

`create_path` and `remove_val` leave an *empty node* materialised at the
location; a location that only ever carried a value does not have one.  The two
are indistinguishable through the public API, yet `remove_branches`,
`take_map`, `join_map_into`, `restrict` and `restricting` all branch on
`get_focus().is_none()` / `try_as_tagged()`, so their return values differ
between them.  `join_map_into` reports `Identity` at a dangling tip and `None` at
a value-only leaf, for instance.

The effects agree in every case observed; only the reported status differs.

## 9. `ascend_until` corrupts a write zipper rooted at a node boundary

`case: ascend_until_wz` — **debug: panic; release: silently wrong reads**

```rust
// dst built by grafting, so [0] lands on a node boundary
let mut wz = map.write_zipper_at_path(&[0]);
wz.descend_last_byte();
wz.ascend_until();
// debug: panicked at src/write_zipper.rs:2802: assertion `left == right` failed
// release: val_count() at the root is wrong afterwards
```

The zipper's node stack is left inconsistent, so subsequent reads through it are
wrong — and a subsequent write would write through the same stack.
`ascend_until_branch` fails the same way.  A plain `reset()` from the same
position is fine, and a `ReadZipper` in the same shape is fine.

## 10. `to_next_k_path` underflows on a borrowed-path zipper

`case: to_next_k_path_borrowed` — **debug: panic; release: wrong branch taken**

```rust
map.insert(b"ab", 1);
let mut rz = map.read_zipper_at_borrowed_path(b"ab");
rz.to_next_k_path(1);
// debug: panicked at src/zipper.rs:2288: attempt to subtract with overflow
```

`path_len()` computes `prefix_buf.len() - origin_path.len()`, and a
borrowed-path zipper has an unallocated `prefix_buf` until something calls
`prepare_buffers()`.  In release the subtraction wraps to ~2^64, so the
`path_len() >= k` test takes the wrong branch.  `read_zipper_at_path` (owned) is
unaffected, and an explicit `prepare_buffers()` first avoids it.

## 11. `to_prev_sibling_byte` asserts on a non-existent focus

`case: prev_sibling_missing` — **debug: panic**

```rust
let mut wz = map.write_zipper();      // empty map
wz.descend_to_byte(0);                // focus does not exist
wz.to_prev_sibling_byte();
// debug: panicked at src/zipper.rs:417: assertion failed: self.path_exists()
```

The `ZipperMoving` default implementation restores the original byte on the
"no previous sibling" path and then asserts the position exists — which it never
did.  Release behaviour (returning `false`) looks correct.

## 12. `remove_unmasked_branches` asserts inside a dangling line node

`case: remove_unmasked_dangling` — **debug: panic**

```rust
map.create_path(&[0]);
let mut wz = map.write_zipper_at_path(&[0]);
wz.remove_unmasked_branches(ByteMask::EMPTY, false);
// debug: panicked at src/line_list_node.rs:1901:
//   assertion failed: !self.is_child_ptr::<0>()
```

## 13. Further arithmetic underflows reached from safe API

Found by the in-process invariant run (400 random programs, release build); not
yet minimised to standalone reproducers, but reproducible from the harness
inputs:

* `src/trie_ref.rs:171` — `range start index 18446744073709551611 out of range
  for slice of length 4`.  A slice range computed by `usize` underflow; hit on
  ~2% of random programs.  It panics rather than reading out of bounds only
  because slice indexing is checked.
* `src/trie_node.rs:3063` — `Attempted to make_unique on an empty sentinel node`.
* `src/write_zipper.rs:1413` — `called Option::unwrap() on a None value`.
* `src/zipper.rs:2801` — `excess_key_len()` underflow, same shape as finding 10.
  Triggered by `fork_read_zipper()` on a write zipper rooted at a non-empty path
  followed by a `to_next_step` traversal.

---

## Two notes that are not bugs, but are worth stating

**`join` is not commutative on values.**  `impl Lattice for u64` (and `usize`,
`u32`, `u16`, `u8`) defines `pjoin` as `Identity(SELF_IDENT)` — a left-biased
projection that ignores the counterpart entirely.  `a.join(&b)` and `b.join(&a)`
therefore agree on the set of paths but disagree on values wherever the two maps
collide.  Likewise `pmeet` keeps `self`'s value even when the two differ.  The
model states the law as commutativity *on locations* only
(`Laws.joinCommOnPaths`); anything asserting value-level commutativity for these
value types is asserting something the crate does not provide.

**`ZipperWriting::restrict` and `PathMap::restrict` differ.**  A path survives
`PathMap::restrict` when *some prefix of it, the empty prefix included*, carries
a value in the right-hand map — so a root value there validates everything.  The
zipper-level `restrict` runs `prestrict` on nodes, and a node has no root value,
so the empty prefix never validates and the source zipper's focus value is
invisible.  The two disagree exactly when the source has a value at its focus.
Both behaviours are modelled (`Map.restrict` versus `Zip.restrict`); it is the
kind of divergence worth either documenting or unifying.
