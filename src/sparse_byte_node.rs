//! [SparseByteNode] is a small byte-keyed node: up to [SPARSE_MAX_ENTRIES] single-byte keys, each
//! with an optional onward link and an optional value, in one block.  It has the semantics of a
//! [DenseByteNode] but replaces the dense node's 64-byte header plus separately allocated `Vec`
//! of 16-byte entries (two blocks, 112 bytes for three entries) with a single 64-byte block for
//! zero-sized values, or 64 bytes plus the values for other value types.
//!
//! Measured on text workloads, 57 to 90 percent of byte nodes hold 3 or 4 entries, so most
//! byte nodes fit.  A node that needs a seventh entry hands back a [DenseByteNode] replacement.
//! Set operations and merges convert to a temporary dense node and use its implementation.
//!
//! Layout (`V` and `A` zero-sized): refcount 4, len 1, value mask 1, child mask 1, pad 1, keys 6,
//! pad 2, six 8-byte child words.  Keys are kept sorted so iteration order matches the dense node.

use core::mem::MaybeUninit;
use core::sync::atomic::AtomicU32;
use std::collections::HashMap;

use crate::alloc::Allocator;
use crate::utils::{BitMask, ByteMask};
use crate::trie_node::*;
use crate::ring::*;
use crate::dense_byte_node::{ByteNode, CoFree, DenseByteNode, OrdinaryCoFree, ALL_BYTES};

/// Most entries a [SparseByteNode] holds
pub const SPARSE_MAX_ENTRIES: usize = 6;

#[repr(C)]
pub struct SparseByteNode<V: Clone + Send + Sync, A: Allocator> {
    #[cfg(feature = "slim_ptrs")]
    refcnt: AtomicU32,
    len: u8,
    /// Bit `i` set: entry `i` has a value in `vals[i]`
    val_mask: u8,
    /// Bit `i` set: entry `i` has an onward link in `children[i]`
    child_mask: u8,
    _pad: u8,
    /// Key bytes of the entries, ascending
    keys: [u8; SPARSE_MAX_ENTRIES],
    _pad2: [u8; 2],
    children: [MaybeUninit<TrieNodeODRc<V, A>>; SPARSE_MAX_ENTRIES],
    vals: [MaybeUninit<V>; SPARSE_MAX_ENTRIES],
    alloc: A,
}

#[cfg(all(feature = "slim_ptrs", target_arch = "x86_64", not(miri)))]
const _: [(); core::mem::size_of::<SparseByteNode<(), crate::alloc::GlobalAlloc>>()] = [(); 64];

unsafe impl<V: Clone + Send + Sync, A: Allocator> Send for SparseByteNode<V, A> {}
unsafe impl<V: Clone + Send + Sync, A: Allocator> Sync for SparseByteNode<V, A> {}

impl<V: Clone + Send + Sync, A: Allocator> Drop for SparseByteNode<V, A> {
    fn drop(&mut self) {
        for i in 0..self.len as usize {
            if self.child_mask & (1 << i) != 0 {
                unsafe { self.children[i].assume_init_drop() }
            }
            if self.val_mask & (1 << i) != 0 {
                unsafe { self.vals[i].assume_init_drop() }
            }
        }
    }
}

impl<V: Clone + Send + Sync, A: Allocator> Clone for SparseByteNode<V, A> {
    fn clone(&self) -> Self {
        let mut new = Self::new_in(self.alloc.clone());
        for i in 0..self.len as usize {
            new.push_entry(self.keys[i], self.child(i).cloned(), self.val(i).cloned());
        }
        new
    }
}

impl<V: Clone + Send + Sync, A: Allocator> core::fmt::Debug for SparseByteNode<V, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SparseByteNode {{count={}", self.len)?;
        for i in 0..self.len as usize {
            write!(f, ", {}:(val={} child={})", self.keys[i], self.has_val(i), self.has_child(i))?;
        }
        write!(f, "}}")
    }
}

impl<V: Clone + Send + Sync, A: Allocator> SparseByteNode<V, A> {
    #[inline]
    pub fn new_in(alloc: A) -> Self {
        Self {
            #[cfg(feature = "slim_ptrs")]
            refcnt: AtomicU32::new(1),
            len: 0,
            val_mask: 0,
            child_mask: 0,
            _pad: 0,
            keys: [0; SPARSE_MAX_ENTRIES],
            _pad2: [0; 2],
            children: [const { MaybeUninit::uninit() }; SPARSE_MAX_ENTRIES],
            vals: [const { MaybeUninit::uninit() }; SPARSE_MAX_ENTRIES],
            alloc,
        }
    }
    /// Moves the entries of a byte node with at most [SPARSE_MAX_ENTRIES] entries into a new sparse node
    pub(crate) fn from_byte_node<Cf: CoFree<V=V, A=A>>(node: ByteNode<Cf, A>, alloc: A) -> Self {
        debug_assert!(node.entry_count() <= SPARSE_MAX_ENTRIES);
        let mut new = Self::new_in(alloc);
        node.into_entries(|k, rec, val| new.push_entry(k, rec, val));
        new
    }
    /// Copies the entries into a [DenseByteNode]
    pub(crate) fn to_dense(&self) -> DenseByteNode<V, A> {
        let mut dense = DenseByteNode::with_capacity_in(self.len as usize, self.alloc.clone());
        for i in 0..self.len as usize {
            dense.set_cofree(self.keys[i], OrdinaryCoFree::new(self.child(i).cloned(), self.val(i).cloned()));
        }
        dense
    }
    /// Moves the entries into a [DenseByteNode], leaving `self` empty
    pub(crate) fn take_into_dense(&mut self) -> DenseByteNode<V, A> {
        let mut dense = DenseByteNode::with_capacity_in(self.len as usize + 1, self.alloc.clone());
        for i in 0..self.len as usize {
            let child = if self.has_child(i) { Some(unsafe { self.children[i].assume_init_read() }) } else { None };
            let val = if self.has_val(i) { Some(unsafe { self.vals[i].assume_init_read() }) } else { None };
            dense.set_cofree(self.keys[i], OrdinaryCoFree::new(child, val));
        }
        self.len = 0;
        self.val_mask = 0;
        self.child_mask = 0;
        dense
    }
    #[inline(always)]
    pub fn entry_count(&self) -> usize { self.len as usize }
    #[inline(always)]
    fn has_val(&self, i: usize) -> bool { self.val_mask & (1 << i) != 0 }
    #[inline(always)]
    fn has_child(&self, i: usize) -> bool { self.child_mask & (1 << i) != 0 }
    #[inline(always)]
    fn child(&self, i: usize) -> Option<&TrieNodeODRc<V, A>> {
        if self.has_child(i) { Some(unsafe { self.children[i].assume_init_ref() }) } else { None }
    }
    #[inline(always)]
    fn child_mut(&mut self, i: usize) -> Option<&mut TrieNodeODRc<V, A>> {
        if self.has_child(i) { Some(unsafe { self.children[i].assume_init_mut() }) } else { None }
    }
    #[inline(always)]
    fn val(&self, i: usize) -> Option<&V> {
        if self.has_val(i) { Some(unsafe { self.vals[i].assume_init_ref() }) } else { None }
    }
    #[inline(always)]
    fn val_mut(&mut self, i: usize) -> Option<&mut V> {
        if self.has_val(i) { Some(unsafe { self.vals[i].assume_init_mut() }) } else { None }
    }
    /// Index of the entry for `k`, or the position where it would be inserted
    #[inline(always)]
    fn find(&self, k: u8) -> Result<usize, usize> {
        let len = self.len as usize;
        for i in 0..len {
            let key = self.keys[i];
            if key == k { return Ok(i); }
            if key > k { return Err(i); }
        }
        Err(len)
    }
    /// Key bytes of the entries
    #[inline(always)]
    fn keys(&self) -> &[u8] { &self.keys[..self.len as usize] }
    /// Appends an entry whose key is greater than every present key
    fn push_entry(&mut self, k: u8, child: Option<TrieNodeODRc<V, A>>, val: Option<V>) {
        let i = self.len as usize;
        debug_assert!(i < SPARSE_MAX_ENTRIES);
        debug_assert!(i == 0 || self.keys[i - 1] < k);
        self.keys[i] = k;
        if let Some(child) = child { self.children[i].write(child); self.child_mask |= 1 << i; }
        if let Some(val) = val { self.vals[i].write(val); self.val_mask |= 1 << i; }
        self.len += 1;
    }
    /// Shifts a mask's bits at and above `i` up by one
    #[inline(always)]
    fn mask_insert(mask: u8, i: usize) -> u8 {
        let low = mask & ((1u8 << i) - 1);
        let high = mask & !((1u8 << i) - 1);
        low | (high << 1)
    }
    /// Removes bit `i` from a mask, shifting the bits above it down
    #[inline(always)]
    fn mask_remove(mask: u8, i: usize) -> u8 {
        let low = mask & ((1u8 << i) - 1);
        let high = mask & !((1u8 << (i + 1)) - 1);
        low | (high >> 1)
    }
    /// Inserts an empty entry for `k` at `i`; the caller guarantees `k` is absent and the node has room
    fn insert_empty(&mut self, i: usize, k: u8) {
        let len = self.len as usize;
        debug_assert!(len < SPARSE_MAX_ENTRIES);
        // Both ends of each copy come from one mutable pointer, so no shared pointer is left dangling
        // when the mutable one is formed
        unsafe {
            let keys = self.keys.as_mut_ptr();
            core::ptr::copy(keys.add(i), keys.add(i + 1), len - i);
            let children = self.children.as_mut_ptr();
            core::ptr::copy(children.add(i), children.add(i + 1), len - i);
            let vals = self.vals.as_mut_ptr();
            core::ptr::copy(vals.add(i), vals.add(i + 1), len - i);
        }
        self.keys[i] = k;
        self.val_mask = Self::mask_insert(self.val_mask, i);
        self.child_mask = Self::mask_insert(self.child_mask, i);
        self.len += 1;
    }
    /// Removes entry `i`, dropping its contents
    fn remove_at(&mut self, i: usize) {
        let len = self.len as usize;
        debug_assert!(i < len);
        if self.has_child(i) { unsafe { self.children[i].assume_init_drop() } }
        if self.has_val(i) { unsafe { self.vals[i].assume_init_drop() } }
        unsafe {
            let keys = self.keys.as_mut_ptr();
            core::ptr::copy(keys.add(i + 1), keys.add(i), len - i - 1);
            let children = self.children.as_mut_ptr();
            core::ptr::copy(children.add(i + 1), children.add(i), len - i - 1);
            let vals = self.vals.as_mut_ptr();
            core::ptr::copy(vals.add(i + 1), vals.add(i), len - i - 1);
        }
        self.val_mask = Self::mask_remove(self.val_mask, i);
        self.child_mask = Self::mask_remove(self.child_mask, i);
        self.len -= 1;
    }
    #[inline]
    fn set_child_at(&mut self, i: usize, node: TrieNodeODRc<V, A>) -> Option<TrieNodeODRc<V, A>> {
        if self.has_child(i) {
            Some(core::mem::replace(unsafe { self.children[i].assume_init_mut() }, node))
        } else {
            self.children[i].write(node);
            self.child_mask |= 1 << i;
            None
        }
    }
    #[inline]
    fn take_child_at(&mut self, i: usize) -> Option<TrieNodeODRc<V, A>> {
        if self.has_child(i) {
            self.child_mask &= !(1 << i);
            Some(unsafe { self.children[i].assume_init_read() })
        } else {
            None
        }
    }
    #[inline]
    fn set_val_at(&mut self, i: usize, val: V) -> Option<V> {
        if self.has_val(i) {
            Some(core::mem::replace(unsafe { self.vals[i].assume_init_mut() }, val))
        } else {
            self.vals[i].write(val);
            self.val_mask |= 1 << i;
            None
        }
    }
    #[inline]
    fn take_val_at(&mut self, i: usize) -> Option<V> {
        if self.has_val(i) {
            self.val_mask &= !(1 << i);
            Some(unsafe { self.vals[i].assume_init_read() })
        } else {
            None
        }
    }
    /// Finds or creates the entry for `k`.  `None` when the node is full and `k` is absent.
    #[inline]
    fn entry_for(&mut self, k: u8) -> Option<usize> {
        match self.find(k) {
            Ok(i) => Some(i),
            Err(i) => {
                if self.len as usize >= SPARSE_MAX_ENTRIES {
                    None
                } else {
                    self.insert_empty(i, k);
                    Some(i)
                }
            }
        }
    }
    /// Builds a dense replacement holding this node's entries, with `f` applied to it
    fn overflow<R>(&mut self, f: impl FnOnce(&mut DenseByteNode<V, A>) -> R) -> (R, TrieNodeODRc<V, A>) {
        let mut dense = self.take_into_dense();
        let r = f(&mut dense);
        (r, TrieNodeODRc::new_in(dense, self.alloc.clone()))
    }
    /// A one-byte child key for `key`, or `None` when the key is longer than a byte
    #[inline(always)]
    fn make_child<F: FnOnce(&mut crate::line_list_node::LineListNode<V, A>)>(&self, f: F) -> TrieNodeODRc<V, A> {
        let mut child = crate::line_list_node::LineListNode::new_in(self.alloc.clone());
        f(&mut child);
        TrieNodeODRc::new_in(child, self.alloc.clone())
    }
}

impl<V: Clone + Send + Sync, A: Allocator> TrieNode<V, A> for SparseByteNode<V, A> {
    #[inline(always)]
    fn node_key_overlap(&self, key: &[u8]) -> usize {
        self.find(key[0]).is_ok() as usize
    }
    #[inline(always)]
    fn node_contains_partial_key(&self, key: &[u8]) -> bool {
        debug_assert!(key.len() > 0);
        key.len() == 1 && self.find(key[0]).is_ok()
    }
    #[inline(always)]
    fn node_get_child(&self, key: &[u8]) -> Option<(usize, &TrieNodeODRc<V, A>)> {
        self.find(key[0]).ok().and_then(|i| self.child(i)).map(|child| (1, child))
    }
    fn node_get_child_mut(&mut self, key: &[u8]) -> Option<(usize, &mut TrieNodeODRc<V, A>)> {
        debug_assert!(key.len() > 0);
        self.find(key[0]).ok().and_then(|i| self.child_mut(i)).map(|child| (1, child))
    }
    fn node_replace_child(&mut self, key: &[u8], new_node: TrieNodeODRc<V, A>) {
        debug_assert!(key.len() == 1);
        let i = self.find(key[0]).unwrap();
        debug_assert!(self.has_child(i));
        *unsafe { self.children[i].assume_init_mut() } = new_node;
    }
    fn node_get_payloads<'node, 'res>(&'node self, keys: &[(&[u8], bool)], results: &'res mut [(usize, PayloadRef<'node, V, A>)]) -> bool {
        // Same algorithm as ByteNode::node_get_payloads, with the requested set tracked per entry index
        let mut unrequested_cofree_half = false;
        let mut stashed_val: Option<&V> = None;
        let mut last_byte: Option<u8> = None;
        let mut unrequested: u8 = if self.len == 0 { 0 } else { ((1u16 << self.len) - 1) as u8 };

        debug_assert!(results.len() >= keys.len());
        for ((key, expect_val), (result_key_len, payload_ref)) in keys.into_iter().zip(results.iter_mut()) {
            if key.len() > 0 {
                let byte = key[0];
                if let Some(prev_byte) = &last_byte {
                    if byte != *prev_byte {
                        if stashed_val.is_some() {
                            unrequested_cofree_half = true;
                        }
                        stashed_val = None;
                        last_byte = None;
                    }
                }
                if let Some(val) = stashed_val {
                    if key.len() == 1 && *expect_val {
                        *result_key_len = 1;
                        *payload_ref = PayloadRef::Val(val);
                        stashed_val = None;
                        continue;
                    }
                }
                if let Ok(i) = self.find(byte) {
                    unrequested &= !(1 << i);
                    if key.len() == 1 && *expect_val && self.has_child(i) {
                        unrequested_cofree_half = true;
                    }
                    if key.len() > 1 || !*expect_val {
                        if let Some(rec) = self.child(i) {
                            *result_key_len = 1;
                            *payload_ref = PayloadRef::Child(rec);
                        }
                    }
                    if let Some(val) = self.val(i) {
                        if key.len() == 1 && *expect_val {
                            debug_assert!(stashed_val.is_none());
                            *result_key_len = 1;
                            *payload_ref = PayloadRef::Val(val);
                        } else if last_byte.is_none() {
                            stashed_val = Some(val);
                            last_byte = Some(byte);
                        }
                    }
                }
            }
        }
        !unrequested_cofree_half && stashed_val.is_none() && unrequested == 0
    }
    fn node_contains_val(&self, key: &[u8]) -> bool {
        key.len() == 1 && self.find(key[0]).map(|i| self.has_val(i)).unwrap_or(false)
    }
    fn node_get_val(&self, key: &[u8]) -> Option<&V> {
        if key.len() == 1 { self.find(key[0]).ok().and_then(|i| self.val(i)) } else { None }
    }
    fn node_get_val_mut(&mut self, key: &[u8]) -> Option<&mut V> {
        if key.len() == 1 { self.find(key[0]).ok().and_then(|i| self.val_mut(i)) } else { None }
    }
    fn node_set_val(&mut self, key: &[u8], val: V) -> Result<(Option<V>, bool), TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        if key.len() > 1 {
            let child = self.make_child(|child| { child.node_set_val(&key[1..], val).unwrap_or_else(|_| panic!()); });
            match self.entry_for(key[0]) {
                Some(i) => { self.set_child_at(i, child); Ok((None, true)) },
                None => { let (_, replacement) = self.overflow(|dense| dense.set_child(key[0], child)); Err(replacement) },
            }
        } else {
            match self.entry_for(key[0]) {
                Some(i) => Ok((self.set_val_at(i, val), false)),
                None => { let (_, replacement) = self.overflow(|dense| dense.set_val(key[0], val)); Err(replacement) },
            }
        }
    }
    fn node_remove_val(&mut self, key: &[u8], prune: bool) -> Option<V> {
        if key.len() != 1 { return None; }
        let i = self.find(key[0]).ok()?;
        let result = self.take_val_at(i);
        if prune && !self.has_child(i) {
            self.remove_at(i);
        }
        result
    }
    fn node_create_dangling(&mut self, key: &[u8]) -> Result<(bool, bool), TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        if key.len() > 1 {
            let child = self.make_child(|child| { child.node_create_dangling(&key[1..]).unwrap_or_else(|_| panic!()); });
            match self.entry_for(key[0]) {
                Some(i) => { self.set_child_at(i, child); Ok((true, true)) },
                None => { let (_, replacement) = self.overflow(|dense| dense.set_child(key[0], child)); Err(replacement) },
            }
        } else {
            match self.find(key[0]) {
                Ok(_) => Ok((false, false)),
                Err(i) => {
                    if self.len as usize >= SPARSE_MAX_ENTRIES {
                        let (_, replacement) = self.overflow(|dense| dense.set_dangling_cf(key[0]));
                        Err(replacement)
                    } else {
                        self.insert_empty(i, key[0]);
                        Ok((true, false))
                    }
                }
            }
        }
    }
    fn node_remove_dangling(&mut self, key: &[u8]) -> usize {
        debug_assert!(key.len() > 0);
        if key.len() == 1 {
            if let Ok(i) = self.find(key[0]) {
                if !self.has_child(i) && !self.has_val(i) {
                    self.remove_at(i);
                    return 1
                }
                if let Some(node) = self.child(i) {
                    if node.as_tagged().node_is_empty() {
                        if self.has_val(i) {
                            drop(self.take_child_at(i));
                        } else {
                            self.remove_at(i);
                            return 1
                        }
                    }
                }
            }
        }
        0
    }
    fn node_set_branch(&mut self, key: &[u8], new_node: TrieNodeODRc<V, A>) -> Result<bool, TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        if key.len() > 1 {
            let child = self.make_child(|child| { child.node_set_branch(&key[1..], new_node).unwrap_or_else(|_| panic!()); });
            match self.entry_for(key[0]) {
                Some(i) => { self.set_child_at(i, child); Ok(true) },
                None => { let (_, replacement) = self.overflow(|dense| dense.set_child(key[0], child)); Err(replacement) },
            }
        } else {
            match self.entry_for(key[0]) {
                Some(i) => { self.set_child_at(i, new_node); Ok(false) },
                None => { let (_, replacement) = self.overflow(|dense| dense.set_child(key[0], new_node)); Err(replacement) },
            }
        }
    }
    fn node_remove_all_branches(&mut self, key: &[u8], prune: bool) -> bool {
        if key.len() > 1 { return false; }
        debug_assert_eq!(key.len(), 1);
        let Ok(i) = self.find(key[0]) else { return false };
        match (self.has_child(i), self.has_val(i)) {
            (true, true) => { drop(self.take_child_at(i)); true },
            (true, false) => {
                if prune { self.remove_at(i); } else { drop(self.take_child_at(i)); }
                true
            },
            (false, _) => false,
        }
    }
    fn node_remove_unmasked_branches(&mut self, key: &[u8], mask: ByteMask, _prune: bool) {
        if key.len() > 0 { return }
        let mut i = 0;
        while i < self.len as usize {
            if mask.test_bit(self.keys[i]) { i += 1; } else { self.remove_at(i); }
        }
    }
    fn node_is_empty(&self) -> bool {
        self.len == 0
    }
    // Iter tokens are entry indices: the token names the next entry to return
    #[inline(always)]
    fn new_iter_token(&self) -> IterToken { 0 }
    #[inline(always)]
    fn iter_token_for_path(&self, key: &[u8]) -> IterToken {
        if key.len() != 1 {
            0
        } else {
            match self.find(key[0]) {
                Ok(i) => (i + 1) as IterToken,
                Err(i) => i as IterToken,
            }
        }
    }
    #[inline(always)]
    fn next_items(&self, token: IterToken) -> (IterToken, &[u8], Option<&TrieNodeODRc<V, A>>, Option<&V>) {
        let i = token as usize;
        if i >= self.len as usize {
            return (NODE_ITER_FINISHED, &[], None, None);
        }
        let k = self.keys[i] as usize;
        ((i + 1) as IterToken, &ALL_BYTES[k..=k], self.child(i), self.val(i))
    }
    fn node_val_count(&self, cache: &mut HashMap<u64, usize>) -> usize {
        (0..self.len as usize).map(|i| self.has_val(i) as usize + self.child(i).map(|r| val_count_below_node(r, cache)).unwrap_or(0)).sum()
    }
    fn node_goat_val_count(&self) -> usize {
        self.val_mask.count_ones() as usize
    }
    fn node_child_iter_start(&self) -> (u64, Option<&TrieNodeODRc<V, A>>) {
        self.node_child_iter_next(0)
    }
    fn node_child_iter_next(&self, token: u64) -> (u64, Option<&TrieNodeODRc<V, A>>) {
        for i in (token as usize)..self.len as usize {
            if let Some(child) = self.child(i) {
                return ((i + 1) as u64, Some(child))
            }
        }
        (0, None)
    }
    #[cfg(feature = "counters")]
    fn item_count(&self) -> usize {
        (self.val_mask.count_ones() + self.child_mask.count_ones()) as usize
    }
    fn node_first_val_depth_along_key(&self, key: &[u8]) -> Option<usize> {
        debug_assert!(key.len() > 0);
        self.find(key[0]).ok().and_then(|i| if self.has_val(i) { Some(0) } else { None })
    }
    fn nth_child_from_key(&self, key: &[u8], n: usize) -> (Option<u8>, Option<TaggedNodeRef<'_, V, A>>) {
        if key.len() > 0 || n >= self.len as usize {
            return (None, None)
        }
        (Some(self.keys[n]), self.child(n).map(|c| c.as_tagged()))
    }
    fn first_child_from_key(&self, key: &[u8]) -> (Option<&[u8]>, Option<TaggedNodeRef<'_, V, A>>) {
        debug_assert_eq!(key.len(), 0);
        debug_assert!(self.len > 0);
        let k = self.keys[0] as usize;
        (Some(&ALL_BYTES[k..=k]), self.child(0).map(|c| c.as_tagged()))
    }
    fn count_branches(&self, key: &[u8]) -> usize {
        match key.len() {
            0 => self.len as usize,
            _ => 0,
        }
    }
    fn node_branches_mask(&self, key: &[u8]) -> ByteMask {
        match key.len() {
            0 => {
                let mut m = ByteMask::EMPTY;
                for k in self.keys() { m.set_bit(*k); }
                m
            },
            _ => ByteMask::EMPTY,
        }
    }
    fn prior_branch_key<'key>(&self, key: &'key [u8]) -> &'key [u8] {
        debug_assert!(key.len() >= 1);
        if key.len() > 1 && self.find(key[0]).is_ok() { &key[0..1] } else { &[] }
    }
    fn get_sibling_of_child(&self, key: &[u8], next: bool) -> (Option<u8>, Option<TaggedNodeRef<'_, V, A>>) {
        if key.len() != 1 {
            return (None, None)
        }
        let Ok(i) = self.find(key[0]) else { return (None, None) };
        let sib = if next { i + 1 } else if i == 0 { return (None, None) } else { i - 1 };
        if sib >= self.len as usize {
            return (None, None)
        }
        (Some(self.keys[sib]), self.child(sib).map(|c| c.as_tagged()))
    }
    fn get_node_at_key(&self, key: &[u8]) -> AbstractNodeRef<'_, V, A> {
        match key.len() {
            0 => if !self.node_is_empty() { AbstractNodeRef::BorrowedDyn(self.as_tagged()) } else { AbstractNodeRef::None },
            1 => match self.find(key[0]).ok().and_then(|i| self.child(i)) {
                Some(link) => AbstractNodeRef::BorrowedRc(link),
                None => AbstractNodeRef::None,
            },
            _ => AbstractNodeRef::None,
        }
    }
    fn take_node_at_key(&mut self, key: &[u8], prune: bool) -> Option<TrieNodeODRc<V, A>> {
        if key.len() != 1 { return None; }
        let i = self.find(key[0]).ok()?;
        let result = self.take_child_at(i);
        if prune && !self.has_val(i) {
            self.remove_at(i);
        }
        result
    }
    fn pjoin_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: Lattice {
        self.to_dense().pjoin_dyn(other)
    }
    fn join_into_dyn(&mut self, other: TrieNodeODRc<V, A>) -> (AlgebraicStatus, Result<(), TrieNodeODRc<V, A>>) where V: Lattice {
        if other.as_tagged().node_is_empty() {
            return (AlgebraicStatus::Identity, Ok(()));
        }
        let mut dense = self.take_into_dense();
        let (status, result) = dense.join_into_dyn(other);
        match result {
            Ok(()) => (status, Err(TrieNodeODRc::from_byte_node(dense, self.alloc.clone()))),
            Err(replacement) => (status, Err(replacement)),
        }
    }
    fn drop_head_dyn(&mut self, byte_cnt: usize) -> Option<TrieNodeODRc<V, A>> where V: Lattice {
        self.take_into_dense().drop_head_dyn(byte_cnt)
    }
    fn pmeet_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: Lattice {
        self.to_dense().pmeet_dyn(other)
    }
    fn psubtract_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: DistributiveLattice {
        self.to_dense().psubtract_dyn(other)
    }
    fn prestrict_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> {
        self.to_dense().prestrict_dyn(other)
    }
    fn clone_self(&self) -> TrieNodeODRc<V, A> {
        TrieNodeODRc::new_in(self.clone(), self.alloc.clone())
    }
}

impl<V: Clone + Send + Sync, A: Allocator> TrieNodeDowncast<V, A> for SparseByteNode<V, A> {
    #[inline]
    fn tag(&self) -> usize {
        SPARSE_BYTE_NODE_TAG
    }
    #[inline]
    fn as_tagged(&self) -> TaggedNodeRef<'_, V, A> {
        TaggedNodeRef::SparseByteNode(self)
    }
    #[cfg(not(feature="slim_ptrs"))]
    fn as_tagged_mut(&mut self) -> TaggedNodeRefMut<'_, V, A> {
        TaggedNodeRefMut::SparseByteNode(self)
    }
    fn convert_to_cell_node(&mut self) -> TrieNodeODRc<V, A> {
        self.take_into_dense().convert_to_cell_node()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathMap;
    use crate::zipper::*;

    fn tag_counts(map: &PathMap<()>) -> HashMap<usize, usize> {
        let mut counts = HashMap::new();
        let mut stack = vec![map.root().unwrap().clone()];
        while let Some(node) = stack.pop() {
            let tagged = node.as_tagged();
            *counts.entry(tagged.tag()).or_insert(0) += 1;
            let (mut token, mut child) = tagged.node_child_iter_start();
            while let Some(c) = child {
                stack.push(c.clone());
                (token, child) = tagged.node_child_iter_next(token);
            }
        }
        counts
    }

    #[test]
    fn masks() {
        assert_eq!(SparseByteNode::<(), ()>::mask_insert(0b0000_1011, 1), 0b0001_0101);
        assert_eq!(SparseByteNode::<(), ()>::mask_insert(0b0000_1011, 0), 0b0001_0110);
        assert_eq!(SparseByteNode::<(), ()>::mask_insert(0b0000_1011, 4), 0b0000_1011);
        assert_eq!(SparseByteNode::<(), ()>::mask_remove(0b0001_0101, 1), 0b0000_1011);
        assert_eq!(SparseByteNode::<(), ()>::mask_remove(0b0001_0110, 0), 0b0000_1011);
        assert_eq!(SparseByteNode::<(), ()>::mask_remove(0b0010_1011, 5), 0b0000_1011);
    }

    #[test]
    fn three_way_split_makes_a_sparse_node() {
        let mut map: PathMap<()> = PathMap::new();
        for k in [&b"ab/1"[..], b"ac/1", b"ad/1"] {
            map.set_val_at(k, ());
        }
        let counts = tag_counts(&map);
        assert_eq!(counts.get(&SPARSE_BYTE_NODE_TAG), Some(&1), "{counts:?}");
        assert_eq!(counts.get(&DENSE_BYTE_NODE_TAG), None, "{counts:?}");
        assert!(map.contains(b"ac/1"));
        assert!(!map.contains(b"ac/"));
        assert!(!map.contains(b"ae/1"));

        // Grow to six entries, still sparse; the seventh converts to a dense node
        for k in [&b"ae/1"[..], b"af/1", b"ag/1"] {
            map.set_val_at(k, ());
        }
        assert_eq!(tag_counts(&map).get(&SPARSE_BYTE_NODE_TAG), Some(&1));
        map.set_val_at(b"ah/1", ());
        let counts = tag_counts(&map);
        assert_eq!(counts.get(&SPARSE_BYTE_NODE_TAG), None, "{counts:?}");
        assert_eq!(counts.get(&DENSE_BYTE_NODE_TAG), Some(&1), "{counts:?}");
        assert_eq!(map.val_count(), 7);
        for k in [&b"ab/1"[..], b"ac/1", b"ad/1", b"ae/1", b"af/1", b"ag/1", b"ah/1"] {
            assert!(map.contains(k));
        }
    }

    #[test]
    fn sparse_node_operations() {
        let mut map: PathMap<()> = PathMap::new();
        for k in [&b"b"[..], b"c", b"d/long_tail_here", b"e/x", b"c/x"] {
            map.set_val_at(k, ());
        }
        let counts = tag_counts(&map);
        assert_eq!(counts.get(&SPARSE_BYTE_NODE_TAG), Some(&1), "{counts:?}");
        assert_eq!(map.val_count(), 5);

        // Navigation
        let mut rz = map.read_zipper();
        assert_eq!(rz.child_count(), 4);
        assert_eq!(rz.child_mask().iter().collect::<Vec<u8>>(), vec![b'b', b'c', b'd', b'e']);
        assert!(rz.descend_indexed_byte(1));
        assert_eq!(rz.path(), b"c");
        assert!(rz.is_val());
        assert_eq!(rz.child_count(), 1);
        assert!(rz.to_next_sibling_byte());
        assert_eq!(rz.path(), b"d");
        assert!(rz.to_prev_sibling_byte());
        assert_eq!(rz.path(), b"c");
        rz.reset();
        let mut paths = vec![];
        while rz.to_next_val() { paths.push(rz.path().to_vec()); }
        assert_eq!(paths, vec![b"b".to_vec(), b"c".to_vec(), b"c/x".to_vec(), b"d/long_tail_here".to_vec(), b"e/x".to_vec()]);
        rz.reset();
        rz.descend_to(b"d/lo");
        assert!(rz.path_exists());
        // "d" has a single child and no value, so the nearest branch point above "d/lo" is the root
        assert!(rz.ascend_until());
        assert_eq!(rz.path(), b"");
        rz.descend_to(b"c/");
        assert!(rz.ascend_until());
        assert_eq!(rz.path(), b"c");
        drop(rz);

        // Removal with and without pruning, dangling paths
        assert_eq!(map.remove_val_at(b"b", true), Some(()));
        assert!(!map.contains(b"b"));
        let mut wz = map.write_zipper_at_path(b"c");
        assert_eq!(wz.remove_val(false), Some(()));
        assert!(wz.path_exists());
        assert_eq!(wz.child_count(), 1);
        wz.set_val(());
        drop(wz);
        let mut wz = map.write_zipper_at_path(b"f");
        assert!(wz.create_path());
        assert!(wz.path_exists());
        assert!(!wz.is_val());
        assert!(wz.prune_path() > 0);
        drop(wz);
        assert_eq!(map.val_count(), 4);
        assert!(!map.path_exists_at(b"f"));
        let mut wz = map.write_zipper_at_path(b"e");
        assert!(wz.remove_branches(true));
        drop(wz);
        assert!(!map.contains(b"e/x"));
        assert_eq!(map.val_count(), 3);

        // Algebra and grafting on both sides
        let mut other: PathMap<()> = PathMap::new();
        for k in [&b"c"[..], b"g/y", b"h"] {
            other.set_val_at(k, ());
        }
        assert_eq!(tag_counts(&other).get(&SPARSE_BYTE_NODE_TAG), Some(&1));
        let joined = map.join(&other);
        assert_eq!(joined.val_count(), 5);
        assert!(joined.contains(b"g/y") && joined.contains(b"c/x"));
        let met = map.meet(&other);
        assert_eq!(met.val_count(), 1);
        let sub = map.subtract(&other);
        assert_eq!(sub.val_count(), 2);
        let mut into = map.clone();
        into.write_zipper().join_into(&other.read_zipper());
        assert_eq!(into.val_count(), 5);
        let mut grafted = map.clone();
        grafted.write_zipper_at_path(b"d").graft_map(other.clone());
        assert!(grafted.contains(b"dg/y"));
        assert!(!grafted.contains(b"d/long_tail_here"));
        let mut restricted = map.clone();
        restricted.write_zipper().restrict(&other.read_zipper());
        assert_eq!(restricted.val_count(), 2);
        assert!(restricted.contains(b"c") && restricted.contains(b"c/x"));

        // Clone-on-write through a shared sparse node
        let snapshot = map.clone();
        map.set_val_at(b"c/y", ());
        assert!(map.contains(b"c/y"));
        assert!(!snapshot.contains(b"c/y"));
        assert_eq!(snapshot.val_count(), 3);
    }

    #[test]
    fn sparse_node_with_values() {
        let mut map: PathMap<u64> = PathMap::new();
        for (i, k) in [&b"a"[..], b"b", b"c", b"c/1", b"d/22"].iter().enumerate() {
            map.set_val_at(k, i as u64);
        }
        assert_eq!(map.get_val_at(b"c"), Some(&2));
        assert_eq!(map.get_val_at(b"d/22"), Some(&4));
        *map.get_val_mut_at(b"b").unwrap() += 10;
        assert_eq!(map.get_val_at(b"b"), Some(&11));
        assert_eq!(map.remove_val_at(b"a", true), Some(0));
        let mut sum = 0;
        let mut rz = map.read_zipper();
        while let Some(v) = rz.to_next_get_val() { sum += *v; }
        assert_eq!(sum, 11 + 2 + 3 + 4);
        assert_eq!(core::mem::size_of::<SparseByteNode<u64, ()>>(), 64 + 6 * 8);
    }
}

