//! Shared behaviour for the two leaf node types that hold exactly one path ending in a zero-sized
//! value: [InlineLeafNode](crate::inline_leaf_node::InlineLeafNode), which lives inside the
//! parent's pointer word, and [CompactLeafNode](crate::compact_leaf_node::CompactLeafNode), a
//! heap block sized to its key.  Both only exist when [leaf_capable] holds.
//!
//! A single-path leaf is either empty, a path with a value at its end, or (after a non-pruning
//! removal) a dangling path whose end is an empty onward link.  The [SinglePathLeaf] trait exposes
//! that state; [impl_single_path_leaf_trie_node] generates the [TrieNode] implementation on top of
//! it, so the traversal and mutation logic exists once.  Writes the storage cannot represent hand
//! back a [LineListNode] replacement, the convention every node type uses when it outgrows itself.

use core::mem::{MaybeUninit, size_of, needs_drop};
use core::ptr::NonNull;

use crate::alloc::Allocator;
use crate::trie_node::*;
use crate::line_list_node::LineListNode;

/// Whether tries of `V` and `A` may hold single-path leaves: the value must occupy no memory and
/// need no drop, and the allocator handle must be zero-sized (the dangling state borrows a shared
/// empty node, which only exists for zero-sized handles)
#[inline(always)]
pub(crate) const fn leaf_capable<V, A>() -> bool {
    size_of::<V>() == 0 && !needs_drop::<V>() && size_of::<A>() == 0
}

/// Produces a value of a zero-sized type.  Sound because a zero-sized type has exactly one bit
/// pattern, the empty one; callers guarantee [leaf_capable].
#[inline(always)]
pub(crate) fn zst<T>() -> T {
    debug_assert_eq!(size_of::<T>(), 0);
    unsafe { MaybeUninit::<T>::uninit().assume_init() }
}
#[inline(always)]
pub(crate) fn zst_ref<'a, T>() -> &'a T {
    debug_assert_eq!(size_of::<T>(), 0);
    unsafe { &*NonNull::<T>::dangling().as_ptr() }
}
#[inline(always)]
pub(crate) fn zst_mut<'a, T>() -> &'a mut T {
    debug_assert_eq!(size_of::<T>(), 0);
    unsafe { &mut *NonNull::<T>::dangling().as_ptr() }
}

/// Storage-independent view of a leaf holding one path
pub(crate) trait SinglePathLeaf<V: Clone + Send + Sync, A: Allocator> {
    /// The path, empty when the leaf is empty
    fn key(&self) -> &[u8];
    /// `true` when the path ends in a value, `false` when it dangles or the leaf is empty
    fn has_val(&self) -> bool;
    /// Marks the path as ending in a value
    fn set_has_val(&mut self);
    /// Removes the value, leaving the path dangling
    fn clear_val(&mut self);
    /// Removes the path entirely
    fn set_empty(&mut self);
    /// Shortens the path to its first `len` bytes and leaves it dangling (`len` is never zero)
    fn truncate_to_dangling(&mut self, len: usize);
    /// The allocator handle (zero-sized, see [leaf_capable])
    fn alloc(&self) -> A;
}

/// Materialises a leaf as a [LineListNode], the heap node that can hold anything a leaf holds and more
pub(crate) fn into_full<V: Clone + Send + Sync, A: Allocator, L: SinglePathLeaf<V, A> + ?Sized>(leaf: &L) -> LineListNode<V, A> {
    let mut node = LineListNode::new_in(leaf.alloc());
    let key = leaf.key();
    if !key.is_empty() {
        let payload = if leaf.has_val() { ValOrChild::Val(zst::<V>()) } else { ValOrChild::Child(TrieNodeODRc::new_empty()) };
        unsafe { node.set_payload_owned::<0>(key, payload); }
    }
    node
}

/// A leaf for the tail of `leaf`'s key from `from` onward, keeping the value state
#[inline]
pub(crate) fn tail_node<V: Clone + Send + Sync, A: Allocator, L: SinglePathLeaf<V, A> + ?Sized>(leaf: &L, from: usize) -> TrieNodeODRc<V, A> {
    TrieNodeODRc::new_single_path_leaf(&leaf.key()[from..], leaf.has_val(), leaf.alloc())
}

/// Runs `f` on a materialised list node and wraps the outcome as a replacement: `Ok` means the list
/// node now holds the result and replaces the leaf, `Err` means the list node itself produced a
/// replacement while performing the operation.
#[inline]
pub(crate) fn replace_with<V: Clone + Send + Sync, A: Allocator, L: SinglePathLeaf<V, A> + ?Sized, R>(leaf: &L, f: impl FnOnce(&mut LineListNode<V, A>) -> Result<R, TrieNodeODRc<V, A>>) -> Result<R, TrieNodeODRc<V, A>> {
    let mut full = into_full(leaf);
    match f(&mut full) {
        Ok(_) => Err(TrieNodeODRc::new_in(full, leaf.alloc())),
        Err(replacement) => Err(replacement),
    }
}

/// Generates the [TrieNode] and [TrieNodeDowncast] implementations for a [SinglePathLeaf] type.
///
/// `$ty` is the node type, `$tag` its pointer tag, `$variant` its [TaggedNodeRef] variant, and
/// `$clone_self` a `fn(&$ty) -> TrieNodeODRc<V, A>` producing an owned copy.
macro_rules! impl_single_path_leaf_trie_node {
    ($ty:ty, $tag:expr, $variant:ident, $clone_self:expr) => {
        impl<V: Clone + Send + Sync, A: Allocator> crate::trie_node::TrieNode<V, A> for $ty {
            fn node_key_overlap(&self, key: &[u8]) -> usize {
                fast_slice_utils::find_prefix_overlap(self.key(), key)
            }
            fn node_contains_partial_key(&self, key: &[u8]) -> bool {
                fast_slice_utils::starts_with(self.key(), key)
            }
            fn node_get_child(&self, key: &[u8]) -> Option<(usize, &crate::trie_node::TrieNodeODRc<V, A>)> {
                let node_key = self.key();
                if !self.has_val() && !node_key.is_empty() && fast_slice_utils::starts_with(key, node_key) {
                    Some((node_key.len(), crate::trie_node::TrieNodeODRc::static_empty()))
                } else {
                    None
                }
            }
            fn node_get_child_mut(&mut self, _key: &[u8]) -> Option<(usize, &mut crate::trie_node::TrieNodeODRc<V, A>)> {
                // A dangling path's onward link is the shared empty sentinel; nothing may write through it.
                // Writers that need a real child go through node_set_branch, which replaces this node.
                None
            }
            fn node_replace_child(&mut self, _key: &[u8], _new_node: crate::trie_node::TrieNodeODRc<V, A>) {
                unreachable!("a single-path leaf has no child to replace")
            }
            fn node_get_payloads<'node, 'res>(&'node self, keys: &[(&[u8], bool)], results: &'res mut [(usize, crate::trie_node::PayloadRef<'node, V, A>)]) -> bool {
                let self_key = self.key();
                if self_key.is_empty() { return true; }
                let mut requested = false;
                debug_assert!(results.len() >= keys.len());
                for ((key, expect_val), (result_key_len, payload_ref)) in keys.iter().zip(results.iter_mut()) {
                    if fast_slice_utils::starts_with(key, self_key) {
                        if self.has_val() {
                            if *expect_val && self_key.len() == key.len() {
                                requested = true;
                                *result_key_len = self_key.len();
                                *payload_ref = crate::trie_node::PayloadRef::Val(crate::single_path_leaf::zst_ref::<V>());
                            }
                        } else if !*expect_val || self_key.len() < key.len() {
                            requested = true;
                            *result_key_len = self_key.len();
                            *payload_ref = crate::trie_node::PayloadRef::Child(crate::trie_node::TrieNodeODRc::static_empty());
                        }
                    }
                }
                requested
            }
            fn node_contains_val(&self, key: &[u8]) -> bool {
                self.has_val() && self.key() == key
            }
            fn node_get_val(&self, key: &[u8]) -> Option<&V> {
                if self.node_contains_val(key) { Some(crate::single_path_leaf::zst_ref::<V>()) } else { None }
            }
            fn node_get_val_mut(&mut self, key: &[u8]) -> Option<&mut V> {
                if self.node_contains_val(key) { Some(crate::single_path_leaf::zst_mut::<V>()) } else { None }
            }
            fn node_set_val(&mut self, key: &[u8], val: V) -> Result<(Option<V>, bool), crate::trie_node::TrieNodeODRc<V, A>> {
                debug_assert!(!key.is_empty());
                if self.key() == key {
                    let had_val = self.has_val();
                    self.set_has_val();
                    core::mem::forget(val);
                    return Ok((had_val.then(crate::single_path_leaf::zst::<V>), false));
                }
                crate::single_path_leaf::replace_with(self, |full| full.node_set_val(key, val))
            }
            fn node_remove_val(&mut self, key: &[u8], prune: bool) -> Option<V> {
                if !self.node_contains_val(key) { return None; }
                if prune { self.set_empty(); } else { self.clear_val(); }
                Some(crate::single_path_leaf::zst::<V>())
            }
            fn node_create_dangling(&mut self, key: &[u8]) -> Result<(bool, bool), crate::trie_node::TrieNodeODRc<V, A>> {
                debug_assert!(!key.is_empty());
                if self.node_contains_partial_key(key) {
                    return Ok((false, false));
                }
                crate::single_path_leaf::replace_with(self, |full| full.node_create_dangling(key))
            }
            fn node_remove_dangling(&mut self, key: &[u8]) -> usize {
                if !self.has_val() && !self.key().is_empty() && self.key() == key {
                    self.set_empty();
                    key.len()
                } else {
                    0
                }
            }
            fn node_set_branch(&mut self, key: &[u8], new_node: crate::trie_node::TrieNodeODRc<V, A>) -> Result<bool, crate::trie_node::TrieNodeODRc<V, A>> {
                crate::single_path_leaf::replace_with(self, |full| full.node_set_branch(key, new_node))
            }
            fn node_remove_all_branches(&mut self, key: &[u8], prune: bool) -> bool {
                // The only branch this node can hold is a dangling path continuing past `key`; a value
                // at `key` itself stays
                let node_key = self.key();
                if !self.has_val() && node_key.len() > key.len() && fast_slice_utils::starts_with(node_key, key) {
                    if prune || key.is_empty() { self.set_empty(); } else { self.truncate_to_dangling(key.len()); }
                    true
                } else {
                    false
                }
            }
            fn node_remove_unmasked_branches(&mut self, key: &[u8], mask: crate::utils::ByteMask, prune: bool) {
                use crate::utils::BitMask;
                let node_key = self.key();
                if node_key.len() > key.len() && fast_slice_utils::starts_with(node_key, key) && !mask.test_bit(node_key[key.len()]) {
                    if prune || key.is_empty() { self.set_empty(); } else { self.truncate_to_dangling(key.len()); }
                }
            }
            fn node_is_empty(&self) -> bool {
                self.key().is_empty()
            }
            fn new_iter_token(&self) -> crate::trie_node::IterToken { 0 }
            fn iter_token_for_path(&self, key: &[u8]) -> crate::trie_node::IterToken {
                if key.is_empty() || key < self.key() { 0 } else if key == self.key() { 1 } else { crate::trie_node::NODE_ITER_FINISHED }
            }
            fn next_items(&self, token: crate::trie_node::IterToken) -> (crate::trie_node::IterToken, &[u8], Option<&crate::trie_node::TrieNodeODRc<V, A>>, Option<&V>) {
                if token == 0 && !self.key().is_empty() {
                    if self.has_val() {
                        (1, self.key(), None, Some(crate::single_path_leaf::zst_ref::<V>()))
                    } else {
                        (1, self.key(), Some(crate::trie_node::TrieNodeODRc::static_empty()), None)
                    }
                } else {
                    (crate::trie_node::NODE_ITER_FINISHED, &[], None, None)
                }
            }
            fn node_val_count(&self, _cache: &mut std::collections::HashMap<u64, usize>) -> usize {
                self.has_val() as usize
            }
            fn node_goat_val_count(&self) -> usize {
                self.has_val() as usize
            }
            fn node_child_iter_start(&self) -> (u64, Option<&crate::trie_node::TrieNodeODRc<V, A>>) {
                if !self.has_val() && !self.key().is_empty() { (0, Some(crate::trie_node::TrieNodeODRc::static_empty())) } else { (0, None) }
            }
            fn node_child_iter_next(&self, _token: u64) -> (u64, Option<&crate::trie_node::TrieNodeODRc<V, A>>) {
                (0, None)
            }
            #[cfg(feature = "counters")]
            fn item_count(&self) -> usize {
                (!self.key().is_empty()) as usize
            }
            fn node_first_val_depth_along_key(&self, key: &[u8]) -> Option<usize> {
                debug_assert!(!key.is_empty());
                let node_key = self.key();
                if self.has_val() && fast_slice_utils::starts_with(key, node_key) { Some(node_key.len() - 1) } else { None }
            }
            fn nth_child_from_key(&self, key: &[u8], n: usize) -> (Option<u8>, Option<crate::trie_node::TaggedNodeRef<'_, V, A>>) {
                let node_key = self.key();
                if n == 0 && node_key.len() > key.len() && fast_slice_utils::starts_with(node_key, key) {
                    let onward = if !self.has_val() && node_key.len() == key.len() + 1 { Some(crate::trie_node::TaggedNodeRef::EmptyNode) } else { None };
                    (Some(node_key[key.len()]), onward)
                } else {
                    (None, None)
                }
            }
            fn first_child_from_key(&self, key: &[u8]) -> (Option<&[u8]>, Option<crate::trie_node::TaggedNodeRef<'_, V, A>>) {
                let node_key = self.key();
                if !node_key.is_empty() && fast_slice_utils::starts_with(node_key, key) {
                    let onward = if self.has_val() { None } else { Some(crate::trie_node::TaggedNodeRef::EmptyNode) };
                    if node_key.len() > key.len() {
                        (Some(&node_key[key.len()..]), onward)
                    } else if self.has_val() {
                        // The key names the whole path and a value ends it
                        (None, None)
                    } else {
                        (Some(&[]), onward)
                    }
                } else {
                    (None, None)
                }
            }
            fn count_branches(&self, key: &[u8]) -> usize {
                let node_key = self.key();
                (node_key.len() > key.len() && fast_slice_utils::starts_with(node_key, key)) as usize
            }
            fn node_branches_mask(&self, key: &[u8]) -> crate::utils::ByteMask {
                use crate::utils::BitMask;
                let node_key = self.key();
                let mut m = crate::utils::ByteMask::EMPTY;
                if node_key.len() > key.len() && fast_slice_utils::starts_with(node_key, key) {
                    m.set_bit(node_key[key.len()]);
                }
                m
            }
            fn prior_branch_key<'key>(&self, key: &'key [u8]) -> &'key [u8] {
                // A single path has no branch points, but a key running past the end of the path stops at the
                // path's end (where the value or dangling link sits), matching LineListNode's behaviour
                debug_assert!(!key.is_empty());
                let node_key = self.key();
                if key.len() > node_key.len() && !node_key.is_empty() && fast_slice_utils::starts_with(key, node_key) {
                    &key[..node_key.len()]
                } else {
                    &[]
                }
            }
            fn get_sibling_of_child(&self, _key: &[u8], _next: bool) -> (Option<u8>, Option<crate::trie_node::TaggedNodeRef<'_, V, A>>) {
                (None, None)
            }
            fn get_node_at_key(&self, key: &[u8]) -> crate::trie_node::AbstractNodeRef<'_, V, A> {
                use crate::trie_node::AbstractNodeRef;
                if key.is_empty() {
                    return if !self.node_is_empty() { AbstractNodeRef::BorrowedDyn(self.as_tagged()) } else { AbstractNodeRef::None };
                }
                let node_key = self.key();
                if node_key.len() > key.len() && fast_slice_utils::starts_with(node_key, key) {
                    return AbstractNodeRef::OwnedRc(crate::single_path_leaf::tail_node(self, key.len()));
                }
                if !self.has_val() && !node_key.is_empty() && node_key == key {
                    return AbstractNodeRef::BorrowedRc(crate::trie_node::TrieNodeODRc::static_empty());
                }
                AbstractNodeRef::None
            }
            fn take_node_at_key(&mut self, key: &[u8], prune: bool) -> Option<crate::trie_node::TrieNodeODRc<V, A>> {
                debug_assert!(!key.is_empty());
                let node_key = self.key();
                if node_key.len() > key.len() && fast_slice_utils::starts_with(node_key, key) {
                    let taken = crate::single_path_leaf::tail_node(self, key.len());
                    if prune { self.set_empty(); } else { self.truncate_to_dangling(key.len()); }
                    return Some(taken);
                }
                if !self.has_val() && !node_key.is_empty() && node_key == key {
                    if prune { self.set_empty(); }
                    return Some(crate::trie_node::TrieNodeODRc::new_empty());
                }
                None
            }
            fn pjoin_dyn(&self, other: crate::trie_node::TaggedNodeRef<V, A>) -> crate::ring::AlgebraicResult<crate::trie_node::TrieNodeODRc<V, A>> where V: crate::ring::Lattice {
                crate::single_path_leaf::into_full(self).pjoin_dyn(other)
            }
            fn join_into_dyn(&mut self, other: crate::trie_node::TrieNodeODRc<V, A>) -> (crate::ring::AlgebraicStatus, Result<(), crate::trie_node::TrieNodeODRc<V, A>>) where V: crate::ring::Lattice {
                if other.as_tagged().node_is_empty() {
                    return (crate::ring::AlgebraicStatus::Identity, Ok(()));
                }
                let mut full = crate::single_path_leaf::into_full(self);
                match full.join_into_dyn(other) {
                    (status, Ok(())) => (status, Err(crate::trie_node::TrieNodeODRc::new_in(full, self.alloc()))),
                    (status, Err(replacement)) => (status, Err(replacement)),
                }
            }
            fn drop_head_dyn(&mut self, byte_cnt: usize) -> Option<crate::trie_node::TrieNodeODRc<V, A>> where V: crate::ring::Lattice {
                debug_assert!(byte_cnt > 0);
                let result = if byte_cnt < self.key().len() {
                    Some(crate::single_path_leaf::tail_node(self, byte_cnt))
                } else {
                    // The whole path is consumed; a value cannot sit at a node root and a dangling path vanishes
                    None
                };
                self.set_empty();
                result
            }
            fn pmeet_dyn(&self, other: crate::trie_node::TaggedNodeRef<V, A>) -> crate::ring::AlgebraicResult<crate::trie_node::TrieNodeODRc<V, A>> where V: crate::ring::Lattice {
                crate::single_path_leaf::into_full(self).pmeet_dyn(other)
            }
            fn psubtract_dyn(&self, other: crate::trie_node::TaggedNodeRef<V, A>) -> crate::ring::AlgebraicResult<crate::trie_node::TrieNodeODRc<V, A>> where V: crate::ring::DistributiveLattice {
                crate::single_path_leaf::into_full(self).psubtract_dyn(other)
            }
            fn prestrict_dyn(&self, other: crate::trie_node::TaggedNodeRef<V, A>) -> crate::ring::AlgebraicResult<crate::trie_node::TrieNodeODRc<V, A>> {
                crate::single_path_leaf::into_full(self).prestrict_dyn(other)
            }
            fn clone_self(&self) -> crate::trie_node::TrieNodeODRc<V, A> {
                ($clone_self)(self)
            }
        }

        impl<V: Clone + Send + Sync, A: Allocator> crate::trie_node::TrieNodeDowncast<V, A> for $ty {
            #[inline]
            fn tag(&self) -> usize {
                $tag
            }
            #[inline]
            fn as_tagged(&self) -> crate::trie_node::TaggedNodeRef<'_, V, A> {
                crate::trie_node::TaggedNodeRef::$variant(self)
            }
            #[cfg(not(feature="slim_ptrs"))]
            fn as_tagged_mut(&mut self) -> crate::trie_node::TaggedNodeRefMut<'_, V, A> {
                unreachable!()
            }
            fn convert_to_cell_node(&mut self) -> crate::trie_node::TrieNodeODRc<V, A> {
                let mut full = crate::single_path_leaf::into_full(self);
                self.set_empty();
                full.convert_to_cell_node()
            }
        }
    };
}
pub(crate) use impl_single_path_leaf_trie_node;
