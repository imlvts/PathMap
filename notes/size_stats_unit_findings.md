# PathMap<()> node footprint: measurements and candidate size optimizations

Measured 2026-09-01 with `cargo run --release --features counters --example size_stats`
(module `src/size_stats.rs`). The structural walk matches a counting global allocator to
within 4 bytes on every dataset, so the per-node-type breakdown below is the real heap.

## Layout today (slim_ptrs, x86_64, V = ())

| type | block | notes |
|---|---|---|
| LineListNode | 64 B, one block | refcnt 4 + key buffer 42 + header 2 + 2 x 8 B payload union |
| DenseByteNode | 64 B header + Vec of 16 B entries | header: refcnt 4 (+4 pad) + 32 B mask + 24 B Vec; entry: `Option<TrieNodeODRc>` 8 B + `Option<()>` 1 B padded to 16 |
| CellByteNode | 64 B header + Vec of 8 B boxes + 16 B per boxed entry | not seen in these workloads |

glibc adds 8 B header + 16 B rounding: a 64 B node costs 80 B. jemalloc has an exact 64 B
class, so with `--features jemalloc` the "requested" column is the real cost.

## Measured (after the Vec clamp below)

| dataset | values | nodes | list | dense | heap B | B/value | key-byte occupancy | chain% | leaf% |
|---|---|---|---|---|---|---|---|---|---|
| cities (asciiname) | 52883 | 50882 | 42002 | 8880 | 4215300 | 79.7 | 6.4% | 7.2% | 62.4% |
| shakespeare words | 35714 | 28787 | 22038 | 6749 | 2409104 | 67.5 | 3.7% | 12.8% | 43.6% |
| shakespeare sentences | 171979 | 205523 | 174815 | 30708 | 16113680 | 93.7 | 17.7% | 17.1% | 51.8% |
| oeis (100k lines) | 99996 | 519978 | 508867 | 11111 | 35411907 | 354.1 | 53.8% | 80.3% | 19.7% |
| big_logic.metta.paths | 91692 | 134486 | 119219 | 15267 | 9578528 | 104.5 | 23.1% | 22.3% | 32.0% |
| seq u64 BE, 1M | 1000000 | 3925 | 1 | 3924 | 16315072 | 16.3 | 6.2% | - | - |
| rand u64, 1M | 1000000 | 1038518 | 972162 | 66356 | 89377408 | 89.4 | 6.7% | 0.0% | 97.1% |
| rand 16 B, 500k | 500000 | 556553 | 491904 | 64649 | 47202016 | 94.4 | 14.8% | 0.0% | 98.3% |
| rand 32 B, 250k | 250000 | 345716 | 297505 | 48211 | 27691360 | 110.8 | 26.8% | 15.9% | 79.9% |
| rand 128 B, 100k | 100000 | 355893 | 342785 | 13108 | 24633968 | 246.3 | 50.4% | 65.9% | 29.2% |

chain% = list nodes holding a single child (path compression); leaf% = list nodes holding a
single value and nothing else. key-byte occupancy = key bytes / requested bytes.

Shape facts that drive the numbers:

- Leaf list nodes dominate short-key data: 62% (cities), 97-98% (random 8-16 B keys). Each is
  a 64 B block (80 B under glibc) for a 1-13 B key and a zero-sized value. The list node's
  42 B key buffer is 6-13% occupied on natural-language and short-key sets.
- Byte nodes are small: 57-90% have 3-4 entries, 85-99% have <= 8. Each costs two blocks
  (64 B header + Vec) for 3-8 key bytes: 25-37 B per key byte.
- Byte node entries are 16 B, half of which is the padded `Option<()>`.
- Long-path data (oeis, 128 B random) is 66-80% chain nodes, mostly with a full 42 B key;
  the list node design works there (87-89% key buffer occupancy) but still pays 64 B per
  42 B of key.
- Before the clamp, full 256-entry byte nodes sat in 384-entry Vecs (with_capacity(3) doubled:
  3, 6, ..., 192, 384). seq_u64 measured 24345280 B; the clamp brings it to 16315072 B (-33%).

## Implemented

P2. `ByteNode::insert_cf` (dense_byte_node.rs) grows the entries Vec by doubling as before
but never past 256 entries. Full nodes now have capacity == 256. Effect: seq_u64 -33%,
rand_16b -1.1%, rand_u64 -0.6%, natural-language sets ~0.

## Candidates, projected from the measured shape histograms

Projected requested bytes as % of the measured layout (`SizeStats::projections`):

| dataset | P1 packed cofree | P3 24 B leaf | P4 64 B sparse | P5 128 B chain (bound) | P6 mask-only leaf | P1+P3+P4 |
|---|---|---|---|---|---|---|
| cities | 88.6% | 75.2% | 88.4% | 100% | 100% | 58.0% |
| shakespeare words | 88.2% | 84.0% | 83.8% | 100% | 99.6% | 64.2% |
| shakespeare sentences | 90.8% | 84.1% | 88.7% | 99.9% | 99.9% | 69.3% |
| oeis | 97.0% | 95.4% | 100% | 82.7% | 100% | 92.4% |
| big_logic paths | 94.9% | 89.4% | 89.9% | 99.7% | 100% | 79.3% |
| seq u64 | 50.8% | 100% | 100% | 100% | 1.9% | 50.8% |
| rand u64 | 87.2% | 57.7% | 99.9% | 100% | 100% | 44.9% |
| rand 16 B | 87.7% | 59.0% | 95.5% | 100% | 100% | 44.5% |
| rand 32 B | 90.0% | 91.4% | 87.7% | 100% | 100% | 75.2% |
| rand 128 B | 96.2% | 99.9% | 96.7% | 88.9% | 100% | 94.5% |

- P1 packed cofree: one 8 B word per entry with the has-value flag in the pointer's spare
  tag bits, `V` stored after it (8 + size_of V). Halves byte-node entry storage for `()`,
  and shrinks `OrdinaryCoFree<u64>` from 24 to 16 B. Obstacle: `prepare_cf`/`both_mut_refs`
  hand out `&mut Option<V>`, which a packed representation cannot produce; those call sites
  need a set/take style API first.
- P3 compact leaf node: a new node type (tags 5-15 are free) of 24 B (refcnt 4, header 1,
  up to 19 key bytes) for a single zero-sized value. Covers 100% of leaves on words, random
  8/16 B keys; 100% on cities (<= 19 B). Biggest single win on short-key data (rand u64:
  -42%). Only pays off when `size_of::<V>() == 0`; with a value slot it becomes 32 B.
- P4 sparse byte node: 64 B single block holding up to 6 (key byte, 8 B entry) pairs, no
  mask, no Vec. Replaces the 3-6 entry byte nodes that are 60-90% of all byte nodes and
  removes one allocation each.
- P5 longer chain nodes: re-pack full 42 B single-child nodes into 112 B keys (128 B block).
  Upper bound 17% on oeis, 11% on 128 B random keys, nothing elsewhere. Variable-length
  node allocation would be the general form.
- P6 mask-only leaf byte node: for `()` a byte node whose entries are all values is fully
  described by its 32 B mask. 1M dense u64 keys: 16.3 MB -> 0.3 MB. Only matters for dense
  integer-like key spaces; nothing on the text sets.
- Allocator: glibc's 8 B header + 16 B rounding is 20-25% on top of every figure above.
  RSS with `--features jemalloc`: rand u64 141 -> 107 MB, rand 16 B 74 -> 57 MB, seq u64
  47 -> 25 MB (per-process runs; small sets are dominated by jemalloc's arena baseline).

Recommended order for `PathMap<()>`: P3 (largest, self-contained new node type), then P4
(reuses the same slot layout), then P1 (API change in `CoFree`). P6 is a special case of the
same leaf-node work if dense integer keys matter.

## Implemented: inline leaves (P7)

Question raised during the work: can a pure value node for `()` be just a bit in the parent?
Only when the leaf's remaining key is empty, and that case already is a flag in the parent's
entry (the `Option<()>` in a byte-node cofree, the value slot in a list node). Every measured
leaf node had 1 to 42 bytes of key left, so those bytes must live somewhere. The parent's
8-byte pointer word is the natural place: `src/inline_leaf_node.rs` stores a leaf with a 1 to
7 byte key and a zero-sized value inside the `TrieNodeODRc` word itself (tag 5, length,
key bytes, a has-value bit), so such a leaf costs nothing beyond the entry it already had.
Enabled automatically when `size_of::<V>() == 0`, `V` needs no drop, and the allocator
handle is zero-sized. Mutations the word cannot hold return a replacement `LineListNode`.

| dataset | before (clamp) | with inline leaves | change | inline leaves | share of nodes |
|---|---|---|---|---|---|
| cities | 4215300 | 2742340 | -35% | 23015 | 45% |
| shakespeare words | 2409104 | 1798224 | -25% | 9545 | 33% |
| shakespeare sentences | 16113680 | 14375888 | -11% | 27153 | 13% |
| oeis | 35411907 | 34304515 | -3% | 17303 | 3% |
| big_logic paths | 9578528 | 8924256 | -7% | 10223 | 8% |
| rand u64 | 89377408 | 28953408 | -68% | 944125 | 91% |
| rand 16 B | 47202016 | 47202016 | 0 (13 B tails) | 0 | 0 |
| rand 128 B | 24633968 | 24613936 | 0 | 313 | 0 |

Build time for 1M random u64 keys dropped from 0.83 s to 0.46 s (fewer allocations); peak
RSS from 165 MB to 90 MB. The 8 to 42 byte leaves (rand 16 B, half of the sentence leaves)
still need the compact heap leaf (P3) or a wider inline form.

Fixed alongside: `LineListNode::set_payload_abstract` propagated a freshly split child's
`Err(replacement)` as its own, which only became reachable once a child could refuse a write.

## Implemented: compact heap leaves (P3, variable size)

`src/compact_leaf_node.rs` holds a single path of 8 to 42 key bytes ending in a zero-sized value
in a heap block sized to the key: an 8-byte header (refcount, flags, logical length, capacity)
followed by the key bytes, rounded up to 8. A 12-byte tail costs 24 bytes instead of 64. The
node type is unsized (trailing byte slice) so its references are fat; the trie's pointer word
stays thin and the capacity is re-read from the header when a reference is formed. The
`TrieNode` implementation is generated by `src/single_path_leaf.rs` and shared with the inline
leaf, so the two differ only in storage. Together they remove every value-only list node from
`PathMap<()>` (leaf% is 0 on all datasets).

| dataset | inline only | inline + compact | change | compact leaves | compact bytes |
|---|---|---|---|---|---|
| cities | 2742340 | 2610884 | -5% | 3177 | 71872 |
| shakespeare words | 1798224 | 1795192 | 0 | 68 | 1320 |
| shakespeare sentences | 14375888 | 12279696 | -15% | 63420 | 1962688 |
| oeis | 34304515 | 32213171 | -6% | 82693 | 3201008 |
| big_logic paths | 8924256 | 8016568 | -10% | 27943 | 880664 |
| rand u64 | 28953408 | 28953408 | 0 | 0 | 0 |
| rand 16 B | 47202016 | 27852656 | -41% | 483734 | 11609616 |
| rand 32 B | 27691360 | 21498680 | -22% | 237715 | 9021080 |
| rand 128 B | 24613936 | 23100128 | -6% | 8171 | 393360 |

Cumulative against the pre-work baseline: cities 4215300 -> 2610884 (-38%), sentences
16113680 -> 12279696 (-24%), rand u64 89377408 -> 28953408 (-68%), rand 16 B 47202016 ->
27852656 (-41%), seq u64 24345280 -> 16315072 (-33%).

Build time and peak RSS for 500k random 16 B keys: 0.40 s / 86 MB -> 0.36 s / 63 MB.

Remaining list nodes are now chains (single child, path compression) and two-payload nodes.
The chain nodes are the next target: a single-path node with an onward link (the same header
plus an 8-byte child pointer after the key) would replace the 64 B list node for short chains.

Found while testing (not fixed, pre-existing on master 2192ebc, reproduces with `usize`
values): grafting at a strict prefix of an existing value's key inside a list node, e.g. at
`abe/tw` below the value `abe/twelve_bytes`, builds a list node with a child at `w` in slot 0
and the value `welve_bytes` in slot 1, which `validate_node` rejects as an ambiguous path.
`tests/graft_repro.rs` holds the ignored repro.
