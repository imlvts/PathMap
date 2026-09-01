//! [CompactLeafNode] is a heap node holding a single path of up to
//! [KEY_BYTES_CNT](crate::line_list_node::KEY_BYTES_CNT) bytes ending in a zero-sized value, in a
//! block sized to the key: an 8-byte header followed by the key bytes, rounded to 8.  A 12-byte
//! tail costs 24 bytes instead of the 64 of a [LineListNode](crate::line_list_node::LineListNode).
//! Paths of 7 bytes or fewer use the [InlineLeafNode](crate::inline_leaf_node::InlineLeafNode)
//! instead, and the node exists only when [leaf_capable](crate::single_path_leaf::leaf_capable) holds.
//!
//! The type is unsized (a trailing byte slice), so a reference to it is a fat pointer whose length
//! is the block's key capacity; the header records the logical key length, so a leaf can shrink in
//! place (`take_node_at_key`, non-pruning removals) but never grow.  The trie's pointer word stays
//! thin: the capacity is re-read from the header when the reference is formed.
//!
//! The [TrieNode] implementation is shared with the inline leaf, see [crate::single_path_leaf].

use core::alloc::Layout;
use core::marker::PhantomData;
use core::sync::atomic::AtomicU32;

use crate::alloc::Allocator;
use crate::trie_node::*;
use crate::line_list_node::{LineListNode, KEY_BYTES_CNT};
use crate::single_path_leaf::{SinglePathLeaf, leaf_capable, zst, impl_single_path_leaf_trie_node};

const FLAG_USED: u8 = 1 << 0;
const FLAG_VAL: u8 = 1 << 1;
/// Offset of the key bytes within the block
const HEADER_BYTES: usize = 8;

/// A heap leaf sized to its key.  See the module documentation.
#[repr(C, align(8))]
pub struct CompactLeafNode<V: Clone + Send + Sync, A: Allocator> {
    /// Reference count, at offset 0 like every heap node so the pointer code can reach it untyped
    refcnt: AtomicU32,
    flags: u8,
    /// Logical key length, at most `key.len()`
    len: u8,
    /// Key capacity of the block; equals `key.len()` and is needed to free the block from a thin pointer
    cap: u8,
    _pad: u8,
    phantom: PhantomData<(V, A)>,
    key: [u8],
}

unsafe impl<V: Clone + Send + Sync, A: Allocator> Send for CompactLeafNode<V, A> {}
unsafe impl<V: Clone + Send + Sync, A: Allocator> Sync for CompactLeafNode<V, A> {}

impl<V: Clone + Send + Sync, A: Allocator> core::fmt::Debug for CompactLeafNode<V, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "CompactLeafNode {{key={:?}, val={}, cap={}}}", self.key(), self.has_val(), self.cap)
    }
}

const _: () = assert!(KEY_BYTES_CNT <= u8::MAX as usize);

impl<V: Clone + Send + Sync, A: Allocator> CompactLeafNode<V, A> {
    /// The block layout for a key of `cap` bytes
    #[inline]
    pub(crate) fn layout_for(cap: usize) -> Layout {
        let size = (HEADER_BYTES + cap + 7) & !7;
        unsafe { Layout::from_size_align_unchecked(size, 8) }
    }
    /// Bytes the block for a key of `cap` bytes occupies
    #[inline]
    pub fn block_size(cap: usize) -> usize {
        Self::layout_for(cap).size()
    }
    /// Allocates a leaf for `key` and returns a thin pointer to its header, with the refcount at 1.
    /// The caller wraps it in a [TrieNodeODRc] under [COMPACT_LEAF_TAG].
    pub(crate) fn alloc_in(key: &[u8], has_val: bool, alloc: &A) -> *mut AtomicU32 {
        debug_assert!(leaf_capable::<V, A>());
        debug_assert!(key.len() >= 1 && key.len() <= KEY_BYTES_CNT);
        let layout = Self::layout_for(key.len());
        #[cfg(not(feature = "nightly"))]
        let ptr = { let _ = alloc; unsafe { std::alloc::alloc(layout) } };
        #[cfg(feature = "nightly")]
        let ptr = match alloc.allocate(layout) { Ok(p) => p.as_ptr().cast::<u8>(), Err(_) => core::ptr::null_mut() };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        unsafe {
            ptr.cast::<AtomicU32>().write(AtomicU32::new(1));
            ptr.add(4).write(FLAG_USED | if has_val { FLAG_VAL } else { 0 });
            ptr.add(5).write(key.len() as u8);
            ptr.add(6).write(key.len() as u8);
            ptr.add(7).write(0);
            core::ptr::copy_nonoverlapping(key.as_ptr(), ptr.add(HEADER_BYTES), key.len());
        }
        ptr.cast()
    }
    /// Frees a block previously produced by [alloc_in](Self::alloc_in), whose refcount has reached zero
    pub(crate) unsafe fn dealloc_in(ptr: *mut AtomicU32, alloc: &A) {
        let cap = unsafe { ptr.cast::<u8>().add(6).read() } as usize;
        let layout = Self::layout_for(cap);
        #[cfg(not(feature = "nightly"))]
        { let _ = alloc; unsafe { std::alloc::dealloc(ptr.cast(), layout) } }
        #[cfg(feature = "nightly")]
        unsafe { alloc.deallocate(core::ptr::NonNull::new_unchecked(ptr.cast()), layout) }
    }
    /// Forms the fat reference for the block at `ptr`, reading the capacity from the header
    #[inline(always)]
    pub(crate) unsafe fn from_thin_ptr<'a>(ptr: *mut AtomicU32) -> &'a Self {
        let cap = unsafe { ptr.cast::<u8>().add(6).read() } as usize;
        unsafe { &*(core::ptr::slice_from_raw_parts(ptr.cast::<u8>(), cap) as *const Self) }
    }
    /// See [from_thin_ptr](Self::from_thin_ptr)
    #[inline(always)]
    pub(crate) unsafe fn from_thin_ptr_mut<'a>(ptr: *mut AtomicU32) -> &'a mut Self {
        let cap = unsafe { ptr.cast::<u8>().add(6).read() } as usize;
        unsafe { &mut *(core::ptr::slice_from_raw_parts_mut(ptr.cast::<u8>(), cap) as *mut Self) }
    }
    /// Key capacity of the block
    #[inline(always)]
    pub fn capacity(&self) -> usize { self.cap as usize }
    /// Bytes this block occupies
    #[inline(always)]
    pub fn size(&self) -> usize { Self::block_size(self.cap as usize) }
    /// Materialises the leaf as a [LineListNode]
    pub(crate) fn into_full(&self) -> LineListNode<V, A> {
        crate::single_path_leaf::into_full(self)
    }
}

impl<V: Clone + Send + Sync, A: Allocator> SinglePathLeaf<V, A> for CompactLeafNode<V, A> {
    #[inline(always)]
    fn key(&self) -> &[u8] {
        if self.flags & FLAG_USED != 0 { &self.key[..self.len as usize] } else { &[] }
    }
    #[inline(always)]
    fn has_val(&self) -> bool { self.flags & FLAG_VAL != 0 }
    #[inline(always)]
    fn set_has_val(&mut self) { self.flags |= FLAG_VAL; }
    #[inline(always)]
    fn clear_val(&mut self) { self.flags &= !FLAG_VAL; }
    #[inline(always)]
    fn set_empty(&mut self) { self.flags = 0; self.len = 0; }
    #[inline]
    fn truncate_to_dangling(&mut self, len: usize) {
        debug_assert!(len >= 1 && len <= self.len as usize);
        self.len = len as u8;
        self.flags = FLAG_USED;
    }
    #[inline(always)]
    fn alloc(&self) -> A { zst::<A>() }
}

impl_single_path_leaf_trie_node!(CompactLeafNode<V, A>, COMPACT_LEAF_TAG, CompactLeaf,
    |node: &CompactLeafNode<V, A>| TrieNodeODRc::new_single_path_leaf(node.key(), node.has_val(), node.alloc()));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathMap;
    use crate::zipper::*;

    fn leaf_tags(map: &PathMap<()>) -> (usize, usize, usize) {
        let (mut inline, mut compact, mut list) = (0, 0, 0);
        let mut stack = vec![map.root().unwrap().clone()];
        while let Some(node) = stack.pop() {
            let tagged = node.as_tagged();
            match tagged.tag() {
                INLINE_LEAF_TAG => inline += 1,
                COMPACT_LEAF_TAG => compact += 1,
                LINE_LIST_NODE_TAG => list += 1,
                _ => {}
            }
            let (mut token, mut child) = tagged.node_child_iter_start();
            while let Some(c) = child {
                stack.push(c.clone());
                (token, child) = tagged.node_child_iter_next(token);
            }
        }
        (inline, compact, list)
    }

    #[test]
    fn block_sizes() {
        assert_eq!(CompactLeafNode::<(), crate::alloc::GlobalAlloc>::block_size(8), 16);
        assert_eq!(CompactLeafNode::<(), crate::alloc::GlobalAlloc>::block_size(16), 24);
        assert_eq!(CompactLeafNode::<(), crate::alloc::GlobalAlloc>::block_size(42), 56);
        let node = TrieNodeODRc::<(), crate::alloc::GlobalAlloc>::new_single_path_leaf(b"twelve_bytes", true, ());
        assert_eq!(node.tag(), COMPACT_LEAF_TAG);
        let tagged = node.as_tagged();
        let leaf = unsafe { tagged.as_compact_unchecked() };
        assert_eq!(leaf.key(), b"twelve_bytes");
        assert!(leaf.has_val());
        assert_eq!(leaf.capacity(), 12);
        assert_eq!(leaf.size(), 24);
        assert_eq!(node.refcount(), 1);
        let copy = node.clone();
        assert_eq!(node.refcount(), 2);
        drop(copy);
        assert_eq!(node.refcount(), 1);
    }

    #[test]
    fn long_tails_become_compact_leaves() {
        let mut map: PathMap<()> = PathMap::new();
        map.set_val_at(b"abc/twelve_bytes", ());
        map.set_val_at(b"abd/twelve_bytes", ());
        map.set_val_at(b"abe/twelve_bytes", ());
        map.set_val_at(b"abf/x", ());
        let (inline, compact, _) = leaf_tags(&map);
        assert_eq!(compact, 3);
        assert_eq!(inline, 1);
        assert!(map.contains(b"abc/twelve_bytes"));
        assert!(!map.contains(b"abc/twelve_byte"));
        assert!(!map.contains(b"abc/twelve_bytes!"));
        let mut paths = vec![];
        let mut rz = map.read_zipper();
        while rz.to_next_val() { paths.push(rz.path().to_vec()); }
        assert_eq!(paths.len(), 4);
        assert_eq!(paths[0], b"abc/twelve_bytes");

        // Extend below a compact leaf (replacement), then remove with and without pruning
        map.set_val_at(b"abc/twelve_bytes/more", ());
        assert!(map.contains(b"abc/twelve_bytes"));
        assert!(map.contains(b"abc/twelve_bytes/more"));
        assert_eq!(map.remove_val_at(b"abd/twelve_bytes", true), Some(()));
        assert!(!map.contains(b"abd/twelve_bytes"));
        let mut wz = map.write_zipper_at_path(b"abe/twelve_bytes");
        assert_eq!(wz.remove_val(false), Some(()));
        assert!(wz.path_exists());
        assert!(!wz.is_val());
        wz.set_val(());
        drop(wz);
        assert_eq!(map.val_count(), 4);

        // Zipper movement across the leaf
        let mut rz = map.read_zipper();
        rz.descend_to(b"abe/twe");
        assert!(rz.path_exists());
        assert_eq!(rz.child_count(), 1);
        assert!(rz.descend_until());
        assert!(rz.is_val());
        assert_eq!(rz.path(), b"abe/twelve_bytes");
        assert!(rz.ascend_until());
        assert!(rz.ascend_until());

        // Algebra with compact leaves on both sides
        let mut other: PathMap<()> = PathMap::new();
        other.set_val_at(b"abc/twelve_bytes", ());
        other.set_val_at(b"abz/another_long_tail", ());
        let joined = map.join(&other);
        assert_eq!(joined.val_count(), 5);
        let met = map.meet(&other);
        assert_eq!(met.val_count(), 1);
        let sub = map.subtract(&other);
        assert_eq!(sub.val_count(), 3);
        // Grafting a subtrie onto a compact leaf's full path and onto a fresh sibling path.  (Grafting
        // strictly below a value's prefix, e.g. at b"abe/tw", trips a pre-existing LineListNode
        // invariant failure that also reproduces without leaf nodes; see tests/graft_repro.rs.)
        let mut grafted = map.clone();
        grafted.write_zipper_at_path(b"abe/twelve_bytes").graft_map(other.clone());
        assert!(grafted.contains(b"abe/twelve_bytesabc/twelve_bytes"));
        assert!(grafted.contains(b"abe/twelve_bytesabz/another_long_tail"));
        assert!(!grafted.contains(b"abe/twelve_bytes"), "graft_root_vals replaces the value at the focus");
        grafted.write_zipper_at_path(b"abg/some_other_tail").graft_map(other.clone());
        assert!(grafted.contains(b"abg/some_other_tailabc/twelve_bytes"));
        assert_eq!(grafted.val_count(), 3 + 2 + 2);
    }
}

