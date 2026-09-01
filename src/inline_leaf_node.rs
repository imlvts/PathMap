//! [InlineLeafNode] is a trie node that occupies no memory of its own: it lives entirely inside the
//! 8-byte pointer word ([TrieNodeODRc]) that a parent would otherwise use to point at a heap node.
//!
//! It represents a single path of 1 to 7 key bytes ending in a zero-sized value (or, after a
//! non-pruning removal, a dangling path with no value).  It exists only when
//! [leaf_capable](crate::single_path_leaf::leaf_capable) holds.  Under those conditions every
//! `PathMap<()>` leaf with a short key costs nothing beyond the parent entry it already occupied.
//! Longer keys use the [CompactLeafNode](crate::compact_leaf_node::CompactLeafNode).
//!
//! Word layout (little-endian, `slim_ptrs` only):
//! - bits 0..56: key bytes, byte `i` at bits `8i..8i+8`
//! - bits 56..59: key length (0 means the node is empty)
//! - bits 59..63: [INLINE_LEAF_TAG], where [SlimNodePtr](crate::trie_node) keeps every node tag
//! - bit 63: set when the path ends in a value, clear when it dangles
//!
//! The [TrieNode] implementation is shared with the compact leaf, see [crate::single_path_leaf].

use core::marker::PhantomData;

use crate::alloc::Allocator;
use crate::trie_node::*;
use crate::line_list_node::LineListNode;
use crate::single_path_leaf::{SinglePathLeaf, leaf_capable, zst, impl_single_path_leaf_trie_node};

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

impl<V: Clone + Send + Sync, A: Allocator> InlineLeafNode<V, A> {
    /// Builds the pointer word for a leaf at `key` (1 to 7 bytes) ending in a value or dangling
    #[inline]
    pub(crate) fn word_for(key: &[u8], has_val: bool) -> u64 {
        debug_assert!(leaf_capable::<V, A>());
        debug_assert!(key.len() >= 1 && key.len() <= INLINE_LEAF_MAX_KEY);
        let mut bytes = [0u8; 8];
        bytes[..key.len()].copy_from_slice(key);
        let mut word = u64::from_le_bytes(bytes);
        word |= (key.len() as u64) << LEN_SHIFT;
        word |= (INLINE_LEAF_TAG as u64) << TAG_SHIFT;
        if has_val { word |= VAL_BIT; }
        word
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
    /// Materialises the leaf as a heap-capable [LineListNode]
    pub(crate) fn into_full(&self) -> LineListNode<V, A> {
        crate::single_path_leaf::into_full(self)
    }
}

impl<V: Clone + Send + Sync, A: Allocator> SinglePathLeaf<V, A> for InlineLeafNode<V, A> {
    #[inline(always)]
    fn key(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), self.key_len()) }
    }
    #[inline(always)]
    fn has_val(&self) -> bool { self.word & VAL_BIT != 0 }
    #[inline(always)]
    fn set_has_val(&mut self) { self.word |= VAL_BIT; }
    #[inline(always)]
    fn clear_val(&mut self) { self.word &= !VAL_BIT; }
    #[inline(always)]
    fn set_empty(&mut self) { self.word = (INLINE_LEAF_TAG as u64) << TAG_SHIFT; }
    #[inline]
    fn truncate_to_dangling(&mut self, len: usize) {
        debug_assert!(len >= 1 && len <= self.key_len());
        self.word = Self::word_for(&self.key()[..len], false);
    }
    #[inline(always)]
    fn alloc(&self) -> A { zst::<A>() }
}

impl_single_path_leaf_trie_node!(InlineLeafNode<V, A>, INLINE_LEAF_TAG, InlineLeaf, |node: &InlineLeafNode<V, A>| TrieNodeODRc::from_inline_word(node.word));

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
        assert!(leaf_capable::<(), crate::alloc::GlobalAlloc>());
        assert!(!leaf_capable::<usize, crate::alloc::GlobalAlloc>());
        assert!(!leaf_capable::<String, crate::alloc::GlobalAlloc>());
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

