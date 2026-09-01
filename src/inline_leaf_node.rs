//! [InlineLeafNode] is a trie node that occupies no memory of its own: it lives entirely inside the
//! 8-byte pointer word ([TrieNodeODRc]) that a parent would otherwise use to point at a heap node.
//!
//! It represents a single path of 1 to 7 key bytes ending in a zero-sized value (or, after a
//! non-pruning removal, a dangling path with no value).  It exists only when
//! [inline_leaf_capable] holds for the trie's `V` and `A` types: the value type is zero-sized and
//! trivially droppable, and the allocator handle is zero-sized.  Under those conditions every
//! `PathMap<()>` leaf with a short key costs nothing beyond the parent entry it already occupied.
//!
//! Word layout (little-endian, `slim_ptrs` only):
//! - bits 0..56: key bytes, byte `i` at bits `8i..8i+8`
//! - bits 56..59: key length (0 means the node is empty)
//! - bits 59..63: [INLINE_LEAF_TAG], where [SlimNodePtr](crate::trie_node) keeps every node tag
//! - bit 63: set when the path ends in a value, clear when it dangles
//!
//! Mutations that cannot be represented in the word return `Err(replacement)` carrying a
//! [LineListNode] with the same contents, following the convention every node type uses when it
//! outgrows its representation.

use core::marker::PhantomData;
use core::mem::{MaybeUninit, size_of, needs_drop};
use core::ptr::NonNull;
use std::collections::HashMap;

use fast_slice_utils::{find_prefix_overlap, starts_with};
use crate::utils::{BitMask, ByteMask};
use crate::alloc::Allocator;
use crate::trie_node::*;
use crate::ring::*;
use crate::line_list_node::LineListNode;

/// Whether tries of `V` and `A` may hold [InlineLeafNode]s
#[inline(always)]
pub(crate) const fn inline_leaf_capable<V, A>() -> bool {
    size_of::<V>() == 0 && !needs_drop::<V>() && size_of::<A>() == 0
}

/// Most key bytes an inline leaf can hold
pub const INLINE_LEAF_MAX_KEY: usize = 7;

const LEN_SHIFT: u32 = 56;
const LEN_MASK: u64 = 0x7 << LEN_SHIFT;
const TAG_SHIFT: u32 = 59;
const VAL_BIT: u64 = 1 << 63;

#[cfg(not(target_endian = "little"))]
compile_error!("InlineLeafNode relies on little-endian byte order to expose its key bytes as a slice");

/// A trie node stored inside a pointer word.  See the module documentation.
#[repr(C, align(8))]
pub struct InlineLeafNode<V: Clone + Send + Sync, A: Allocator> {
    word: u64,
    phantom: PhantomData<(V, A)>,
}

impl<V: Clone + Send + Sync, A: Allocator> Clone for InlineLeafNode<V, A> {
    fn clone(&self) -> Self { Self { word: self.word, phantom: PhantomData } }
}
impl<V: Clone + Send + Sync, A: Allocator> Copy for InlineLeafNode<V, A> {}
unsafe impl<V: Clone + Send + Sync, A: Allocator> Send for InlineLeafNode<V, A> {}
unsafe impl<V: Clone + Send + Sync, A: Allocator> Sync for InlineLeafNode<V, A> {}

impl<V: Clone + Send + Sync, A: Allocator> core::fmt::Debug for InlineLeafNode<V, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "InlineLeafNode {{key={:?}, val={}}}", self.key(), self.has_val())
    }
}

/// Produces a value of a zero-sized type.  Sound because a zero-sized type has exactly one bit
/// pattern, the empty one; callers guarantee [inline_leaf_capable].
#[inline(always)]
fn zst<T>() -> T {
    debug_assert_eq!(size_of::<T>(), 0);
    unsafe { MaybeUninit::<T>::uninit().assume_init() }
}
#[inline(always)]
fn zst_ref<'a, T>() -> &'a T {
    debug_assert_eq!(size_of::<T>(), 0);
    unsafe { &*NonNull::<T>::dangling().as_ptr() }
}
#[inline(always)]
fn zst_mut<'a, T>() -> &'a mut T {
    debug_assert_eq!(size_of::<T>(), 0);
    unsafe { &mut *NonNull::<T>::dangling().as_ptr() }
}

impl<V: Clone + Send + Sync, A: Allocator> InlineLeafNode<V, A> {
    /// Builds the pointer word for a leaf at `key` (1 to 7 bytes) ending in a value or dangling
    #[inline]
    pub(crate) fn word_for(key: &[u8], has_val: bool) -> u64 {
        debug_assert!(inline_leaf_capable::<V, A>());
        debug_assert!(key.len() >= 1 && key.len() <= INLINE_LEAF_MAX_KEY);
        let mut bytes = [0u8; 8];
        bytes[..key.len()].copy_from_slice(key);
        let mut word = u64::from_le_bytes(bytes);
        word |= (key.len() as u64) << LEN_SHIFT;
        word |= (INLINE_LEAF_TAG as u64) << TAG_SHIFT;
        if has_val { word |= VAL_BIT; }
        word
    }
    /// Returns the pointer word for `list` if it is a single value at a 1 to 7 byte key, or `None`
    #[inline]
    pub(crate) fn word_for_list(list: &LineListNode<V, A>) -> Option<u64> {
        if !inline_leaf_capable::<V, A>() { return None; }
        if list.is_used::<1>() || !list.is_used_value_0() { return None; }
        let (key0, _) = list.get_both_keys();
        if key0.len() > INLINE_LEAF_MAX_KEY { return None; }
        Some(Self::word_for(key0, true))
    }
    /// Reinterprets a pointer word as a leaf.  The caller guarantees the word carries [INLINE_LEAF_TAG].
    #[inline(always)]
    pub(crate) unsafe fn from_word_ref<'a>(word: &'a u64) -> &'a Self {
        unsafe { &*(word as *const u64).cast::<Self>() }
    }
    /// See [from_word_ref](Self::from_word_ref)
    #[inline(always)]
    pub(crate) unsafe fn from_word_mut<'a>(word: &'a mut u64) -> &'a mut Self {
        unsafe { &mut *(word as *mut u64).cast::<Self>() }
    }
    #[inline(always)]
    pub fn key_len(&self) -> usize { ((self.word & LEN_MASK) >> LEN_SHIFT) as usize }
    #[inline(always)]
    pub fn has_val(&self) -> bool { self.word & VAL_BIT != 0 }
    #[inline(always)]
    pub fn key(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), self.key_len()) }
    }
    #[inline(always)]
    fn alloc(&self) -> A { zst::<A>() }
    #[inline(always)]
    fn set_empty(&mut self) {
        self.word = (INLINE_LEAF_TAG as u64) << TAG_SHIFT;
    }
    #[inline(always)]
    fn clear_val(&mut self) {
        self.word &= !VAL_BIT;
    }
    /// Materialises the leaf as a heap-capable [LineListNode]
    pub(crate) fn into_full(&self) -> LineListNode<V, A> {
        let mut node = LineListNode::new_in(self.alloc());
        if self.key_len() > 0 {
            let payload = if self.has_val() { ValOrChild::Val(zst::<V>()) } else { ValOrChild::Child(TrieNodeODRc::new_empty()) };
            unsafe { node.set_payload_owned::<0>(self.key(), payload); }
        }
        node
    }
    /// A leaf for the tail of this leaf's key from `from` onward, keeping the value state
    #[inline]
    fn tail_node(&self, from: usize) -> TrieNodeODRc<V, A> {
        TrieNodeODRc::new_inline_leaf(&self.key()[from..], self.has_val())
    }
    /// A reference to an empty node, for the onward link of a dangling path
    #[inline(always)]
    fn empty_child(&self) -> &TrieNodeODRc<V, A> {
        TrieNodeODRc::static_empty()
    }
    /// Runs `f` on a materialised list node and wraps the outcome as a replacement: `Ok` means the
    /// list node now holds the result and replaces this leaf, `Err` means the list node itself
    /// produced a replacement while performing the operation.
    #[inline]
    fn replace_with<R>(&self, f: impl FnOnce(&mut LineListNode<V, A>) -> Result<R, TrieNodeODRc<V, A>>) -> Result<R, TrieNodeODRc<V, A>> {
        let mut full = self.into_full();
        match f(&mut full) {
            Ok(_) => Err(TrieNodeODRc::new_in(full, self.alloc())),
            Err(replacement) => Err(replacement),
        }
    }
}

impl<V: Clone + Send + Sync, A: Allocator> TrieNode<V, A> for InlineLeafNode<V, A> {
    fn node_key_overlap(&self, key: &[u8]) -> usize {
        find_prefix_overlap(self.key(), key)
    }
    fn node_contains_partial_key(&self, key: &[u8]) -> bool {
        starts_with(self.key(), key)
    }
    fn node_get_child(&self, key: &[u8]) -> Option<(usize, &TrieNodeODRc<V, A>)> {
        if !self.has_val() && self.key_len() > 0 && starts_with(key, self.key()) {
            Some((self.key_len(), self.empty_child()))
        } else {
            None
        }
    }
    fn node_get_child_mut(&mut self, _key: &[u8]) -> Option<(usize, &mut TrieNodeODRc<V, A>)> {
        // A dangling path's onward link is the shared empty sentinel; nothing may write through it.
        // Writers that need a real child go through node_set_branch, which replaces this node.
        None
    }
    fn node_replace_child(&mut self, _key: &[u8], _new_node: TrieNodeODRc<V, A>) { unreachable!("an inline leaf has no child to replace") }
    fn node_get_payloads<'node, 'res>(&'node self, keys: &[(&[u8], bool)], results: &'res mut [(usize, PayloadRef<'node, V, A>)]) -> bool {
        if self.key_len() == 0 { return true; }
        let mut requested = false;
        let self_key = self.key();
        debug_assert!(results.len() >= keys.len());
        for ((key, expect_val), (result_key_len, payload_ref)) in keys.iter().zip(results.iter_mut()) {
            if starts_with(key, self_key) {
                if self.has_val() {
                    if *expect_val && self_key.len() == key.len() {
                        requested = true;
                        *result_key_len = self_key.len();
                        *payload_ref = PayloadRef::Val(zst_ref::<V>());
                    }
                } else if !*expect_val || self_key.len() < key.len() {
                    requested = true;
                    *result_key_len = self_key.len();
                    *payload_ref = PayloadRef::Child(self.empty_child());
                }
            }
        }
        requested
    }
    fn node_contains_val(&self, key: &[u8]) -> bool {
        self.has_val() && self.key() == key
    }
    fn node_get_val(&self, key: &[u8]) -> Option<&V> {
        if self.node_contains_val(key) { Some(zst_ref::<V>()) } else { None }
    }
    fn node_get_val_mut(&mut self, key: &[u8]) -> Option<&mut V> {
        if self.node_contains_val(key) { Some(zst_mut::<V>()) } else { None }
    }
    fn node_set_val(&mut self, key: &[u8], val: V) -> Result<(Option<V>, bool), TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        if self.key() == key {
            let had_val = self.has_val();
            self.word |= VAL_BIT;
            core::mem::forget(val);
            return Ok((had_val.then(zst::<V>), false));
        }
        self.replace_with(|full| full.node_set_val(key, val))
    }
    fn node_remove_val(&mut self, key: &[u8], prune: bool) -> Option<V> {
        if !self.node_contains_val(key) { return None; }
        if prune { self.set_empty(); } else { self.clear_val(); }
        Some(zst::<V>())
    }
    fn node_create_dangling(&mut self, key: &[u8]) -> Result<(bool, bool), TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        if self.node_contains_partial_key(key) {
            return Ok((false, false));
        }
        self.replace_with(|full| full.node_create_dangling(key))
    }
    fn node_remove_dangling(&mut self, key: &[u8]) -> usize {
        if !self.has_val() && self.key_len() > 0 && self.key() == key {
            self.set_empty();
            key.len()
        } else {
            0
        }
    }
    fn node_set_branch(&mut self, key: &[u8], new_node: TrieNodeODRc<V, A>) -> Result<bool, TrieNodeODRc<V, A>> {
        self.replace_with(|full| full.node_set_branch(key, new_node))
    }
    fn node_remove_all_branches(&mut self, key: &[u8], prune: bool) -> bool {
        // The only branch this node can hold is a dangling path; a value at `key` stays.
        if !self.has_val() && self.key_len() > 0 && starts_with(self.key(), key) && self.key_len() > key.len() {
            if prune { self.set_empty(); } else { self.word = Self::word_for(key, false) | (0 & VAL_BIT); if key.is_empty() { self.set_empty(); } }
            true
        } else if !self.has_val() && self.key_len() > 0 && self.key() == key {
            // Removing the branches below a dangling path's end: nothing is below it
            false
        } else {
            false
        }
    }
    fn node_remove_unmasked_branches(&mut self, key: &[u8], mask: ByteMask, prune: bool) {
        if self.key_len() > key.len() && starts_with(self.key(), key) && !mask.test_bit(self.key()[key.len()]) {
            if self.has_val() || prune { self.set_empty(); } else { self.node_remove_all_branches(key, prune); }
        }
    }
    fn node_is_empty(&self) -> bool {
        self.key_len() == 0
    }
    fn new_iter_token(&self) -> IterToken { 0 }
    fn iter_token_for_path(&self, key: &[u8]) -> IterToken {
        if key.len() == 0 || key < self.key() { 0 } else if key == self.key() { 1 } else { NODE_ITER_FINISHED }
    }
    fn next_items(&self, token: IterToken) -> (IterToken, &[u8], Option<&TrieNodeODRc<V, A>>, Option<&V>) {
        if token == 0 && self.key_len() > 0 {
            if self.has_val() {
                (1, self.key(), None, Some(zst_ref::<V>()))
            } else {
                (1, self.key(), Some(self.empty_child()), None)
            }
        } else {
            (NODE_ITER_FINISHED, &[], None, None)
        }
    }
    fn node_val_count(&self, _cache: &mut HashMap<u64, usize>) -> usize {
        self.has_val() as usize
    }
    fn node_goat_val_count(&self) -> usize {
        self.has_val() as usize
    }
    fn node_child_iter_start(&self) -> (u64, Option<&TrieNodeODRc<V, A>>) {
        if !self.has_val() && self.key_len() > 0 { (0, Some(self.empty_child())) } else { (0, None) }
    }
    fn node_child_iter_next(&self, _token: u64) -> (u64, Option<&TrieNodeODRc<V, A>>) {
        (0, None)
    }
    #[cfg(feature = "counters")]
    fn item_count(&self) -> usize {
        (self.key_len() > 0) as usize
    }
    fn node_first_val_depth_along_key(&self, key: &[u8]) -> Option<usize> {
        debug_assert!(key.len() > 0);
        if self.has_val() && starts_with(key, self.key()) { Some(self.key_len() - 1) } else { None }
    }
    fn nth_child_from_key(&self, key: &[u8], n: usize) -> (Option<u8>, Option<TaggedNodeRef<'_, V, A>>) {
        let node_key = self.key();
        if n == 0 && node_key.len() > key.len() && starts_with(node_key, key) {
            let onward = if !self.has_val() && node_key.len() == key.len() + 1 { Some(TaggedNodeRef::EmptyNode) } else { None };
            (Some(node_key[key.len()]), onward)
        } else {
            (None, None)
        }
    }
    fn first_child_from_key(&self, key: &[u8]) -> (Option<&[u8]>, Option<TaggedNodeRef<'_, V, A>>) {
        let node_key = self.key();
        if node_key.len() > 0 && starts_with(node_key, key) {
            let onward = if self.has_val() { None } else { Some(TaggedNodeRef::EmptyNode) };
            if node_key.len() > key.len() {
                (Some(&node_key[key.len()..]), onward)
            } else {
                // The key names the whole path; a value ends it, a dangling path has an empty onward node
                if self.has_val() { (None, None) } else { (Some(&[]), onward) }
            }
        } else {
            (None, None)
        }
    }
    fn count_branches(&self, key: &[u8]) -> usize {
        let node_key = self.key();
        (node_key.len() > key.len() && starts_with(node_key, key)) as usize
    }
    fn node_branches_mask(&self, key: &[u8]) -> ByteMask {
        let node_key = self.key();
        let mut m = ByteMask::EMPTY;
        if node_key.len() > key.len() && starts_with(node_key, key) {
            m.set_bit(node_key[key.len()]);
        }
        m
    }
    fn prior_branch_key<'key>(&self, key: &'key [u8]) -> &'key [u8] {
        // A single path has no branch points, but a key running past the end of the path stops at the
        // path's end (where the value or dangling link sits), matching LineListNode's behaviour
        debug_assert!(key.len() > 0);
        let node_key = self.key();
        if key.len() > node_key.len() && node_key.len() > 0 && starts_with(key, node_key) {
            &key[..node_key.len()]
        } else {
            &[]
        }
    }
    fn get_sibling_of_child(&self, _key: &[u8], _next: bool) -> (Option<u8>, Option<TaggedNodeRef<'_, V, A>>) {
        (None, None)
    }
    fn get_node_at_key(&self, key: &[u8]) -> AbstractNodeRef<'_, V, A> {
        if key.len() == 0 {
            return if !self.node_is_empty() { AbstractNodeRef::BorrowedDyn(self.as_tagged()) } else { AbstractNodeRef::None };
        }
        let node_key = self.key();
        if node_key.len() > key.len() && starts_with(node_key, key) {
            return AbstractNodeRef::OwnedRc(self.tail_node(key.len()));
        }
        if !self.has_val() && node_key.len() > 0 && node_key == key {
            return AbstractNodeRef::BorrowedRc(self.empty_child());
        }
        AbstractNodeRef::None
    }
    fn take_node_at_key(&mut self, key: &[u8], prune: bool) -> Option<TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        let node_key = self.key();
        if node_key.len() > key.len() && starts_with(node_key, key) {
            let taken = self.tail_node(key.len());
            if prune { self.set_empty(); } else { self.word = Self::word_for(key, false); }
            return Some(taken);
        }
        if !self.has_val() && node_key.len() > 0 && node_key == key {
            if prune { self.set_empty(); }
            return Some(TrieNodeODRc::new_empty());
        }
        None
    }
    fn pjoin_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: Lattice {
        self.into_full().pjoin_dyn(other)
    }
    fn join_into_dyn(&mut self, other: TrieNodeODRc<V, A>) -> (AlgebraicStatus, Result<(), TrieNodeODRc<V, A>>) where V: Lattice {
        if other.as_tagged().node_is_empty() {
            return (AlgebraicStatus::Identity, Ok(()));
        }
        let mut full = self.into_full();
        match full.join_into_dyn(other) {
            (status, Ok(())) => (status, Err(TrieNodeODRc::new_in(full, self.alloc()))),
            (status, Err(replacement)) => (status, Err(replacement)),
        }
    }
    fn drop_head_dyn(&mut self, byte_cnt: usize) -> Option<TrieNodeODRc<V, A>> where V: Lattice {
        debug_assert!(byte_cnt > 0);
        let key_len = self.key_len();
        if byte_cnt < key_len {
            let result = self.tail_node(byte_cnt);
            self.set_empty();
            Some(result)
        } else {
            // The whole path is consumed; a value cannot sit at a node root and a dangling path vanishes
            self.set_empty();
            None
        }
    }
    fn pmeet_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: Lattice {
        self.into_full().pmeet_dyn(other)
    }
    fn psubtract_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: DistributiveLattice {
        self.into_full().psubtract_dyn(other)
    }
    fn prestrict_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> {
        self.into_full().prestrict_dyn(other)
    }
    fn clone_self(&self) -> TrieNodeODRc<V, A> {
        TrieNodeODRc::from_inline_word(self.word)
    }
}

impl<V: Clone + Send + Sync, A: Allocator> TrieNodeDowncast<V, A> for InlineLeafNode<V, A> {
    #[inline]
    fn tag(&self) -> usize {
        INLINE_LEAF_TAG
    }
    #[inline]
    fn as_tagged(&self) -> TaggedNodeRef<'_, V, A> {
        TaggedNodeRef::InlineLeaf(self)
    }
    #[cfg(not(feature="slim_ptrs"))]
    fn as_tagged_mut(&mut self) -> TaggedNodeRefMut<'_, V, A> {
        unreachable!()
    }
    fn convert_to_cell_node(&mut self) -> TrieNodeODRc<V, A> {
        let mut full = self.into_full();
        self.set_empty();
        full.convert_to_cell_node()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathMap;
    use crate::zipper::*;

    #[test]
    fn word_round_trip() {
        let word = InlineLeafNode::<(), crate::alloc::GlobalAlloc>::word_for(b"hello", true);
        let node = unsafe { InlineLeafNode::<(), crate::alloc::GlobalAlloc>::from_word_ref(&word) };
        assert_eq!(node.key(), b"hello");
        assert!(node.has_val());
        assert_eq!(node.tag(), INLINE_LEAF_TAG);
        assert_eq!((word >> TAG_SHIFT) & 0xF, INLINE_LEAF_TAG as u64);
        assert!(inline_leaf_capable::<(), crate::alloc::GlobalAlloc>());
        assert!(!inline_leaf_capable::<usize, crate::alloc::GlobalAlloc>());
        assert!(!inline_leaf_capable::<String, crate::alloc::GlobalAlloc>());
    }

    #[test]
    fn unit_leaves_are_inlined() {
        let mut map: PathMap<()> = PathMap::new();
        // Three siblings force a byte node, and each sibling has a 3-byte tail: three leaf nodes
        map.set_val_at(b"abc/xy", ());
        map.set_val_at(b"abd/xy", ());
        map.set_val_at(b"abe/xy", ());
        map.set_val_at(b"rubens", ());
        assert_eq!(map.val_count(), 4);
        assert!(map.contains(b"abc/xy"));
        assert!(map.contains(b"rubens"));
        assert!(!map.contains(b"abc"));
        assert!(!map.contains(b"abc/x"));
        let mut paths: Vec<Vec<u8>> = vec![];
        let mut rz = map.read_zipper();
        while rz.to_next_val() {
            paths.push(rz.path().to_vec());
        }
        assert_eq!(paths, vec![b"abc/xy".to_vec(), b"abd/xy".to_vec(), b"abe/xy".to_vec(), b"rubens".to_vec()]);

        // The short leaves must have been folded into their parents' pointer words
        let mut inline_leaves = 0;
        let mut stack = vec![map.root().unwrap().clone()];
        while let Some(node) = stack.pop() {
            if node.as_tagged().tag() == INLINE_LEAF_TAG { inline_leaves += 1; }
            let tagged = node.as_tagged();
            let (mut token, mut child) = tagged.node_child_iter_start();
            while let Some(c) = child {
                stack.push(c.clone());
                (token, child) = tagged.node_child_iter_next(token);
            }
        }
        assert!(inline_leaves >= 3, "expected the value leaves to be inline, found {inline_leaves}");
    }

    #[test]
    fn mutations_through_inline_leaves() {
        let mut map: PathMap<()> = PathMap::new();
        for k in [&b"abc"[..], b"abd", b"abe", b"xyz"] {
            map.set_val_at(k, ());
        }
        // Extend below an inline leaf: forces a replacement
        map.set_val_at(b"abcdef", ());
        assert!(map.contains(b"abc"));
        assert!(map.contains(b"abcdef"));
        // Remove with and without pruning
        assert_eq!(map.remove_val_at(b"abd", true), Some(()));
        assert!(!map.contains(b"abd"));
        let mut wz = map.write_zipper_at_path(b"abe");
        assert_eq!(wz.remove_val(false), Some(()));
        assert!(wz.path_exists(), "a non-pruning removal leaves the path dangling");
        assert!(!wz.is_val());
        wz.set_val(());
        assert!(wz.is_val());
        drop(wz);
        assert_eq!(map.val_count(), 4);
        // Re-insert and iterate everything
        map.set_val_at(b"abd", ());
        let mut count = 0;
        let mut rz = map.read_zipper();
        while rz.to_next_val() { count += 1; }
        assert_eq!(count, 5);
        // Graft a subtrie onto a leaf's path and join two maps holding inline leaves
        let mut other: PathMap<()> = PathMap::new();
        other.set_val_at(b"abc", ());
        other.set_val_at(b"abz", ());
        other.set_val_at(b"q", ());
        let joined = map.join(&other);
        assert_eq!(joined.val_count(), 7);
        let met = map.meet(&other);
        assert_eq!(met.val_count(), 1);
        assert!(met.contains(b"abc"));
        let sub = map.subtract(&other);
        assert_eq!(sub.val_count(), 4);
        assert!(!sub.contains(b"abc"));
    }
}

