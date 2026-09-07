
//GOAT: Internal Discussion: What do we ultimately want this API to look like?
//
// * Firstly, we probably ought to make a writable trait, in the vein of `ZipperSubtries` that allows
// merkleization on any object that has a mutable trie node (PathMaps, WriteZippers, etc.)
// I've wanted that anyway for a while because I felt like a lot of the API between ZipperWriting and
// PathMap should be merged together.
//
// * Secondly, I think we ought to allow the client to exfiltrate and store the `memo`, wrapped in some
// kind of black box.  That way, they could perform "gradual merkleization", as an opportunistic background
// process.
//
// However, the `memo` is liable to get huge in real-world situations, therefore we also want some kind of
// way to implement a heuristic to evict nodes that are unlikely to be duplicated.  I'm not totally sure what
// that heuristic(s) would be.  My first guess would be to try an LRU cache, but also, higher nodes are less
// likely to be identical, but also we get more "bang" if we find higher-level sharing.  So perhaps finding
// a that's shared means we make sure its parents are refreshed into the cache...  Anyway, gotta try stuff
// and measure.
//
// One additional consideration if we go the persistent memo route is that the very existence of the memo
// structure, under the present implementation, would increase the node refcounts and lead to copying... So
// we might want to explore something like a "weak" pointer to keep in the memo (semantics wouldn't be exactly
// the same as an Rc `weak`, but a similar idea)

use core::convert::Infallible;
use core::hash::{Hash, Hasher};
use std::collections::hash_map::Entry;

use crate::alloc::Allocator;
use crate::gxhash;
use crate::morphisms::trie_hash;
use crate::trie_node::*;
use crate::utils::ByteMask;

/// Statistics created after merkleization
#[derive(Default, Debug)]
pub struct MerkleizeResult {
    /// The hash of the entire trie beneath the root, equal to [`CatamorphismCached::hash`](crate::morphisms::CatamorphismCached::hash)
    pub hash: u128,
    /// The number of shared node references that replaced identical copies during the merkleization
    pub reused: usize,
    //GOAT, not sure how to describe this
    pub cloned: usize,
    //GOAT, not sure how to describe this
    pub replaced: usize,
}

/// Merkleizes the trie below `root`, returning the statistics and the replacement for the root node
/// if it was rewritten
///
/// Nodes are identified by the hash of the **logical** trie below them, computed by the cached
/// catamorphism with the same algebra as [`CatamorphismCached::hash`](crate::morphisms::CatamorphismCached::hash).
/// Two nodes therefore merge whenever they hold the same paths and values, regardless of how those
/// paths are laid out in nodes and key segments, and regardless of the value the parent stores for
/// the node's root position.
///
/// The cata records the value-free hash of every node, in the order it finishes them.  A second,
/// bottom-up pass over the physical nodes then replaces every child whose hash was seen before with
/// the first node that produced that hash.
pub(crate) fn merkleize_root<V, A>(root: &TrieNodeODRc<V, A>, root_val: Option<&V>) -> (MerkleizeResult, Option<TrieNodeODRc<V, A>>)
    where
        V: Clone + Send + Sync + Hash,
        A: Allocator,
{
    let mut result = MerkleizeResult::default();

    let mut hashes = NodeHashes::default();
    let start_f = |bm: &ByteMask| Ok::<_, Infallible>(trie_hash::node_hasher(bm));
    let fold_child_f = |_bm: &ByteMask, child: u128, hasher: &mut gxhash::GxHasher| { hasher.write_u128(child); Ok(()) };
    let leaf = trie_hash::leaf();
    let step_up = |mut w: u128, prefix: &[u8]| { for byte in prefix.iter().rev() { w = trie_hash::step(*byte, w); } w };
    let map_f = |value: &V, prefix: &[u8]| Ok(step_up(trie_hash::with_value(trie_hash::value_hash(value), leaf), prefix));
    let finalize_f = |bm: &ByteMask, hasher: Option<gxhash::GxHasher>, prefix: &[u8]| Ok(step_up(hasher.unwrap_or_else(|| trie_hash::node_hasher(bm)).finish_u128(), prefix));
    let collapse_f = |value: &V, below: u128| Ok(trie_hash::with_value(trie_hash::value_hash(value), below));
    result.hash = match root_val {
        Some(val) => collapse_slot_val::<_, _, _, _, _, _, _, _, _, _, _, true>(root, val, start_f, fold_child_f, map_f, finalize_f, collapse_f, &mut hashes),
        None => recursive_cata_cached::<_, _, _, _, _, _, _, _, _, _, _, true>(root, start_f, fold_child_f, map_f, finalize_f, collapse_f, &mut hashes),
    }.unwrap_or_else(|never| match never {});

    let mut memo = gxhash::HashMap::<u128, TrieNodeODRc<V, A>>::default();
    let mut visited = gxhash::HashMap::<u64, Option<TrieNodeODRc<V, A>>>::default();
    let new_root = merkleize_impl(&mut result, &mut hashes, &mut memo, &mut visited, root);
    (result, new_root)
}

/// The cata cache used for merkleization: every node's hash is recorded, in the order the cata
/// finishes nodes, and nodes that are physically shared are additionally indexed by address so
/// the cata summarizes them once
#[derive(Default)]
struct NodeHashes {
    /// `(address, hash)` for every node the cata finished, in post-order
    order: Vec<(u64, u128)>,
    /// Hashes of nodes with more than one parent, so the cata can reuse them
    shared: gxhash::HashMap<u64, u128>,
    /// Read position in `order` for the rewrite pass
    cursor: usize,
    /// Built from `order` if the rewrite pass ever visits nodes in a different order than the cata did
    by_address: Option<gxhash::HashMap<u64, u128>>,
}

impl<V: Clone + Send + Sync, A: Allocator> CataCache<V, A, u128> for NodeHashes {
    #[inline]
    fn skips(&self, _node: &TrieNodeODRc<V, A>) -> bool {
        false
    }
    #[inline]
    fn caches(&self, node: &TrieNodeODRc<V, A>) -> bool {
        !node.is_empty()
    }
    #[inline]
    fn get(&self, key: u64) -> Option<&u128> {
        self.shared.get(&key)
    }
    #[inline]
    fn insert(&mut self, key: u64, node: &TrieNodeODRc<V, A>, w: &u128) {
        self.order.push((key, *w));
        if node.refcount() > 1 {
            self.shared.insert(key, *w);
        }
    }
}

impl NodeHashes {
    /// Whether the node at `address` had more than one parent when the cata ran
    #[inline]
    fn is_shared(&self, address: u64) -> bool {
        !self.shared.is_empty() && self.shared.contains_key(&address)
    }
    /// The hash recorded for the node at `address`, which the rewrite pass reaches in the cata's order
    #[inline]
    fn take(&mut self, address: u64) -> Option<u128> {
        if let Some(by_address) = &self.by_address {
            return by_address.get(&address).copied();
        }
        if let Some(&(recorded, hash)) = self.order.get(self.cursor) {
            if recorded == address {
                self.cursor += 1;
                return Some(hash);
            }
        }
        // The traversal orders diverged; the remaining lookups go through an index
        let by_address = self.order.iter().copied().collect::<gxhash::HashMap<u64, u128>>();
        let hash = by_address.get(&address).copied();
        self.by_address = Some(by_address);
        hash
    }
}

/// Rewrites the trie below `node`, returning the node the parent must store in its slot instead of
/// `node`, if any
fn merkleize_impl<V, A>(
    counters: &mut MerkleizeResult,
    hashes: &mut NodeHashes,
    memo: &mut gxhash::HashMap<u128, TrieNodeODRc<V, A>>,
    visited: &mut gxhash::HashMap<u64, Option<TrieNodeODRc<V, A>>>,
    node: &TrieNodeODRc<V, A>,
) -> Option<TrieNodeODRc<V, A>>
    where
        V: Clone + Send + Sync,
        A: Allocator,
{
    // The cata does not visit the empty node
    if node.is_empty() {
        return None;
    }
    let address = node.shared_node_id();
    // A node with more than one parent is rewritten once, and every parent gets the same answer.  Only
    // such nodes can be reached twice, so only they are tracked.  The cata recorded which nodes those
    // are; refcounts are no guide here because rewriting a parent shares its children with the copy
    let shared = hashes.is_shared(address);
    if shared {
        if let Some(previous) = visited.get(&address) {
            return previous.clone();
        }
    }

    let mut replacement = None;
    let node_ref = node.as_tagged();
    let mut it = node_ref.new_iter_token();
    while it != NODE_ITER_FINISHED {
        let (next, path, child, _val) = node_ref.next_items(it);
        it = next;
        if let Some(child) = child {
            if let Some(replace) = merkleize_impl(counters, hashes, memo, visited, child) {
                let node = replacement.get_or_insert_with(|| {
                    counters.cloned += 1;
                    node.clone()
                });
                counters.replaced += 1;
                node.make_mut().node_replace_child(path, replace);
            }
        }
    }

    let hash = hashes.take(address).expect("the cata visits every non-empty node");
    match memo.entry(hash) {
        Entry::Vacant(entry) => {
            entry.insert(replacement.clone().unwrap_or_else(|| node.clone()));
        },
        // Another node with the same logical contents was seen first: point the parent at it
        Entry::Occupied(entry) => {
            counters.reused += 1;
            replacement = Some(entry.get().clone());
        },
    }
    if shared {
        visited.insert(address, replacement.clone());
    }
    replacement
}

#[cfg(test)]
mod tests {

    /// Regression test: the memo key must not include the value stored in the *parent's* slot for
    /// a child, otherwise byte-identical nodes reached through a valued slot and an unvalued slot are
    /// never merged.
    ///
    /// The trie contains every bitstring over {0, 1} of length 0..=4 that ends in `1`, plus the
    /// empty path.  The subtrie below byte `0` and the subtrie below byte `1` are identical, except
    /// that the path `[1]` carries a value, which lives in the root node's slot, not in the child.
    /// So after merkleization there should be exactly one physical node per depth: 4 in total.
    #[cfg(feature="viz")]
    #[test]
    fn merkleize_parent_slot_value_should_not_split_identical_children() {
        use crate::viz::{viz_maps, DrawConfig, VizMode};

        // All paths over {0,1} of length 1..=4 whose last byte is 1, plus the empty path
        let mut paths: Vec<Vec<u8>> = vec![vec![]];
        for len in 1..=4usize {
            for bits in 0..(1u32 << len) {
                let path: Vec<u8> = (0..len).map(|i| ((bits >> (len - 1 - i)) & 1) as u8).collect();
                if *path.last().unwrap() == 1 {
                    paths.push(path);
                }
            }
        }
        assert_eq!(paths.len(), 16);
        let mut map = crate::PathMap::from_iter(paths.iter().map(|p| (p.as_slice(), ())));
        assert_eq!(map.val_count(), 16);

        let count_physical_nodes = |map: &crate::PathMap<()>| {
            let dc = DrawConfig{ mode: VizMode::Mermaid, ascii_path: false, hide_value_paths: true, minimize_values: true, logical: false, color: false };
            let mut out = Vec::new();
            viz_maps(std::slice::from_ref(map), &dc, &mut out).unwrap();
            let out = String::from_utf8(out).unwrap();
            // `viz_node_physical` emits one `shape: rect` line per distinct node address
            let n = out.lines().filter(|l| l.contains("shape: rect")).count();
            (n, out)
        };

        let (before, _) = count_physical_nodes(&map);
        let result = map.merkleize();
        let (after, rendered) = count_physical_nodes(&map);
        eprintln!("physical nodes before merkleize: {before}, after: {after}\nmerkleize result: {result:?}\n```mermaid\n{rendered}```");
        assert_eq!(map.val_count(), 16, "merkleize must not change the logical contents");
        assert_eq!(after, 4, "expected one physical node per depth after merkleize, got {after}");
    }

    use std::collections::HashSet;
    use crate::PathMap;
    use crate::morphisms::CatamorphismCached;
    use crate::trie_node::{TrieNodeODRc, NODE_ITER_FINISHED};
    use crate::zipper::*;

    /// Counts the distinct physical nodes reachable from the map root
    fn physical_node_count(map: &PathMap<()>) -> usize {
        fn visit(node: &TrieNodeODRc<(), crate::alloc::GlobalAlloc>, seen: &mut HashSet<u64>) {
            if node.is_empty() || !seen.insert(node.shared_node_id()) {
                return;
            }
            let node_ref = node.as_tagged();
            let mut it = node_ref.new_iter_token();
            while it != NODE_ITER_FINISHED {
                let (next, _path, child, _val) = node_ref.next_items(it);
                it = next;
                if let Some(child) = child {
                    visit(child, seen);
                }
            }
        }
        let mut seen = HashSet::new();
        if let Some(root) = map.root() {
            visit(root, &mut seen);
        }
        seen.len()
    }

    fn all_paths(map: &PathMap<()>) -> Vec<Vec<u8>> {
        let mut rz = map.read_zipper();
        let mut paths = Vec::new();
        while rz.to_next_val() {
            paths.push(rz.path().to_vec());
        }
        paths
    }

    /// The same four paths, either inserted directly (`abcd`/`abce` become `a` -> `bc` -> `{d, e}`)
    /// or assembled by grafting `{cd, ce}` under `ab` (which leaves `a` -> `b` -> `c` -> `{d, e}`)
    fn insert_copy(map: &mut PathMap<()>, prefix: &[u8], grafted: bool) {
        let mut wz = map.write_zipper_at_path(prefix);
        if grafted {
            let sub: PathMap<()> = [b"cd".as_slice(), b"ce"].into_iter().map(|leaf| (leaf, ())).collect();
            wz.descend_to(b"ab");
            wz.graft_map(sub);
            wz.reset();
            for path in [b"m".as_slice(), b"n"] {
                wz.descend_to(path);
                wz.set_val(());
                wz.reset();
            }
        } else {
            for path in [b"abcd".as_slice(), b"abce", b"m", b"n"] {
                wz.descend_to(path);
                wz.set_val(());
                wz.reset();
            }
        }
    }

    #[test]
    fn merkleize_hash_is_the_cata_hash_and_keeps_the_contents() {
        let mut map = PathMap::<()>::new();
        insert_copy(&mut map, b"1", false);
        insert_copy(&mut map, b"2", true);
        map.set_val_at(b"1", ());
        let paths = all_paths(&map);
        let hash = CatamorphismCached::hash(&map);

        let result = map.merkleize();

        assert_eq!(result.hash, hash);
        assert_eq!(all_paths(&map), paths);
        assert_eq!(CatamorphismCached::hash(&map), hash);
    }

    #[test]
    fn merkleize_merges_logically_equal_subtries_with_different_layouts() {
        let mut direct = PathMap::<()>::new();
        insert_copy(&mut direct, b"", false);
        let mut split = PathMap::<()>::new();
        insert_copy(&mut split, b"", true);
        let split = split;
        assert_eq!(all_paths(&direct), all_paths(&split));
        let (direct_nodes, grafted_nodes) = (physical_node_count(&direct), physical_node_count(&split));
        assert_ne!(direct_nodes, grafted_nodes, "the two copies should differ in layout for this test to mean anything");

        let mut map = PathMap::<()>::new();
        insert_copy(&mut map, b"1", false);
        insert_copy(&mut map, b"2", true);
        let before = physical_node_count(&map);
        assert_eq!(before, 1 + direct_nodes + grafted_nodes);

        let result = map.merkleize();
        let after = physical_node_count(&map);
        eprintln!("direct copy: {direct_nodes} nodes, grafted copy: {grafted_nodes} nodes, before: {before}, after: {after}, {result:?}");
        // The second copy collapses onto the first, node for node, so only the root is left on top
        // of one copy (whose own internal duplicates are merged as well)
        direct.merkleize();
        assert_eq!(after, 1 + physical_node_count(&direct));
        assert!(result.reused >= 1);
    }

    #[test]
    fn merkleize_is_idempotent_and_leaves_shared_nodes_alone() {
        let mut map = PathMap::<()>::new();
        for prefix in [b"1".as_slice(), b"2", b"3"] {
            insert_copy(&mut map, prefix, prefix == b"2");
        }
        let first = map.merkleize();
        assert!(first.reused > 0);
        let after_first = physical_node_count(&map);

        let second = map.merkleize();
        assert_eq!(second.hash, first.hash);
        assert_eq!((second.reused, second.replaced, second.cloned), (0, 0, 0));
        assert_eq!(physical_node_count(&map), after_first);

        // A trie that already shares its nodes physically is left untouched too
        let mut grafted = PathMap::<()>::new();
        let mut shared = PathMap::<()>::new();
        insert_copy(&mut shared, b"", false);
        for prefix in [b"1".as_slice(), b"2", b"3"] {
            let mut wz = grafted.write_zipper_at_path(prefix);
            wz.graft_map(shared.clone());
        }
        let before = physical_node_count(&grafted);
        let result = grafted.merkleize();
        assert_eq!((result.reused, result.replaced, result.cloned), (0, 0, 0));
        assert_eq!(physical_node_count(&grafted), before);
    }

    #[test]
    fn test_btm_merkleize() {
        let paths: &[&[u8]] = &[
            b"axx",
            b"ayy",
            b"bxx",
            b"byy",
            b"cxx",
            b"cyy",
            b"ddxx",
            b"ddyy",
        ];
        let paths = paths.iter()
            .map(|&path| (path, ()));
        let mut btm = crate::PathMap::from_iter(paths);
        #[cfg(feature="viz")] {
            let mut before = Vec::new();
            use crate::viz::{viz_maps, DrawConfig};
            viz_maps(&[btm.clone()], &DrawConfig::default(), &mut before).unwrap();
            eprintln!("before:");
            eprintln!("```mermaid\n{}```", std::str::from_utf8(&before).unwrap());
        }
        let result = btm.merkleize();
        eprintln!("merkleize result: {result:?}\n");
        #[cfg(feature="viz")] {
            use crate::viz::{viz_maps, DrawConfig};
            let mut after = Vec::new();
            viz_maps(&[btm], &DrawConfig::default(), &mut after).unwrap();
            eprintln!("after:");
            eprintln!("```mermaid\n{}```", std::str::from_utf8(&after).unwrap());
        }
    }
}