//! [UnitByteNode] is a byte-keyed node for zero-sized values: a child mask, a value mask and a
//! `Vec` of bare child pointers.  Values occupy no memory at all and an all-value node needs no
//! `Vec`.  It replaces the [DenseByteNode] (whose 16-byte entries are half padding when `V` is
//! zero-sized) for unit tries once a node outgrows the [SparseByteNode](crate::sparse_byte_node),
//! and exists only when [leaf_capable](crate::single_path_leaf::leaf_capable) holds.
//!
//! Block cost: 96 bytes plus 8 per child (a 256-entry all-value node is 96 bytes; a dense node is
//! 64 plus 4096).  The join, meet, subtract and restrict operations run natively on the masks when
//! both sides are unit byte nodes; the value lattice is consulted once per operation because every
//! zero-sized value is identical.  Anything else converts to a temporary dense node.
//!
//! A dangling path (created by `create_path`) is a child entry holding the empty sentinel.

use core::marker::PhantomData;
use core::sync::atomic::AtomicU32;
use std::collections::HashMap;

use crate::alloc::Allocator;
use crate::utils::{BitMask, ByteMask};
use crate::trie_node::*;
use crate::ring::*;
use crate::dense_byte_node::{ByteNode, CoFree, DenseByteNode, OrdinaryCoFree, ALL_BYTES};
use crate::single_path_leaf::{leaf_capable, zst, zst_ref, zst_mut};

#[repr(C)]
pub struct UnitByteNode<V: Clone + Send + Sync, A: Allocator> {
    #[cfg(feature = "slim_ptrs")]
    refcnt: AtomicU32,
    /// Entries with an onward link; `children` is ordered by these bits
    child_mask: ByteMask,
    /// Entries with a value
    val_mask: ByteMask,
    children: Vec<TrieNodeODRc<V, A>>,
    alloc: A,
    phantom: PhantomData<V>,
}

#[cfg(all(feature = "slim_ptrs", target_arch = "x86_64", not(miri)))]
const _: [(); core::mem::size_of::<UnitByteNode<(), crate::alloc::GlobalAlloc>>()] = [(); 96];

unsafe impl<V: Clone + Send + Sync, A: Allocator> Send for UnitByteNode<V, A> {}
unsafe impl<V: Clone + Send + Sync, A: Allocator> Sync for UnitByteNode<V, A> {}

impl<V: Clone + Send + Sync, A: Allocator> Clone for UnitByteNode<V, A> {
    fn clone(&self) -> Self {
        Self::new_with_fields_in(self.child_mask, self.val_mask, self.children.clone(), self.alloc.clone())
    }
}

impl<V: Clone + Send + Sync, A: Allocator> core::fmt::Debug for UnitByteNode<V, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "UnitByteNode {{count={}", self.entry_count())?;
        for k in self.entry_mask().iter() {
            write!(f, ", {k}:(val={} child={})", self.val_mask.test_bit(k), self.child_mask.test_bit(k))?;
        }
        write!(f, "}}")
    }
}

/// A borrowed entry: onward link and value presence
type Entry<'a, V, A> = (Option<&'a TrieNodeODRc<V, A>>, bool);
/// An owned entry
type OwnedEntry<V, A> = (Option<TrieNodeODRc<V, A>>, bool);

impl<V: Clone + Send + Sync, A: Allocator> UnitByteNode<V, A> {
    #[inline]
    pub fn new_in(alloc: A) -> Self {
        Self::new_with_fields_in(ByteMask::EMPTY, ByteMask::EMPTY, Vec::new(), alloc)
    }
    #[inline]
    fn new_with_fields_in(child_mask: ByteMask, val_mask: ByteMask, children: Vec<TrieNodeODRc<V, A>>, alloc: A) -> Self {
        debug_assert!(leaf_capable::<V, A>());
        debug_assert_eq!(child_mask.count_bits(), children.len());
        Self {
            #[cfg(feature = "slim_ptrs")]
            refcnt: AtomicU32::new(1),
            child_mask,
            val_mask,
            children,
            alloc,
            phantom: PhantomData,
        }
    }
    /// Moves the entries of a byte node into a new unit byte node
    pub(crate) fn from_byte_node<Cf: CoFree<V=V, A=A>>(node: ByteNode<Cf, A>, alloc: A) -> Self {
        let mut new = Self::new_in(alloc);
        new.children.reserve_exact(node.child_count());
        node.into_entries(|k, rec, val| {
            if let Some(rec) = rec { new.child_mask.set_bit(k); new.children.push(rec); }
            if val.is_some() { new.val_mask.set_bit(k); }
        });
        new
    }
    /// Copies the entries into a [DenseByteNode]
    pub(crate) fn to_dense(&self) -> DenseByteNode<V, A> {
        let mut dense = DenseByteNode::with_capacity_in(self.entry_count(), self.alloc.clone());
        for k in self.entry_mask().iter() {
            dense.set_cofree(k, OrdinaryCoFree::new(self.child(k).cloned(), self.val_mask.test_bit(k).then(zst::<V>)));
        }
        dense
    }
    /// Moves the entries into a [DenseByteNode], leaving `self` empty
    pub(crate) fn take_into_dense(&mut self) -> DenseByteNode<V, A> {
        let mut dense = DenseByteNode::with_capacity_in(self.entry_count() + 1, self.alloc.clone());
        let children = core::mem::take(&mut self.children);
        let mut children = children.into_iter();
        for k in self.entry_mask().iter() {
            let child = if self.child_mask.test_bit(k) { children.next() } else { None };
            dense.set_cofree(k, OrdinaryCoFree::new(child, self.val_mask.test_bit(k).then(zst::<V>)));
        }
        self.child_mask = ByteMask::EMPTY;
        self.val_mask = ByteMask::EMPTY;
        dense
    }
    #[inline(always)]
    pub fn entry_mask(&self) -> ByteMask { self.child_mask | self.val_mask }
    #[inline(always)]
    pub fn entry_count(&self) -> usize { self.entry_mask().count_bits() }
    #[inline(always)]
    pub fn child_count(&self) -> usize { self.children.len() }
    /// Length and capacity of the child pointer vector
    #[inline(always)]
    pub fn children_len_capacity(&self) -> (usize, usize) { (self.children.len(), self.children.capacity()) }
    #[inline(always)]
    pub fn val_count(&self) -> usize { self.val_mask.count_bits() }
    #[inline(always)]
    fn has_entry(&self, k: u8) -> bool { self.child_mask.test_bit(k) || self.val_mask.test_bit(k) }
    #[inline(always)]
    fn child(&self, k: u8) -> Option<&TrieNodeODRc<V, A>> {
        if self.child_mask.test_bit(k) { Some(unsafe { self.children.get_unchecked(self.child_mask.index_of(k) as usize) }) } else { None }
    }
    #[inline(always)]
    fn child_mut(&mut self, k: u8) -> Option<&mut TrieNodeODRc<V, A>> {
        if self.child_mask.test_bit(k) { let ix = self.child_mask.index_of(k) as usize; Some(unsafe { self.children.get_unchecked_mut(ix) }) } else { None }
    }
    #[inline(always)]
    fn entry(&self, k: u8) -> Entry<'_, V, A> {
        (self.child(k), self.val_mask.test_bit(k))
    }
    /// Inserts `node` at `ix`, growing the vector without overshooting 256 entries (see `ByteNode::insert_cf`)
    #[inline]
    fn insert_child_at(&mut self, ix: usize, node: TrieNodeODRc<V, A>) {
        let len = self.children.len();
        if len == self.children.capacity() {
            let additional = (256 - len).min(if len == 0 { 4 } else { len });
            self.children.reserve_exact(additional);
        }
        self.children.insert(ix, node);
    }
    /// Sets the onward link for `k`, returning the previous one
    fn set_child(&mut self, k: u8, node: TrieNodeODRc<V, A>) -> Option<TrieNodeODRc<V, A>> {
        let ix = self.child_mask.index_of(k) as usize;
        if self.child_mask.test_bit(k) {
            Some(core::mem::replace(&mut self.children[ix], node))
        } else {
            self.child_mask.set_bit(k);
            self.insert_child_at(ix, node);
            None
        }
    }
    fn take_child(&mut self, k: u8) -> Option<TrieNodeODRc<V, A>> {
        if self.child_mask.test_bit(k) {
            let ix = self.child_mask.index_of(k) as usize;
            self.child_mask.clear_bit(k);
            Some(self.children.remove(ix))
        } else {
            None
        }
    }
    /// Removes the whole entry for `k`
    fn remove_entry(&mut self, k: u8) {
        drop(self.take_child(k));
        self.val_mask.clear_bit(k);
    }
    /// Sets the whole entry for `k`, which must be absent
    fn push_entry(&mut self, k: u8, entry: OwnedEntry<V, A>) {
        debug_assert!(!self.has_entry(k));
        if let Some(child) = entry.0 { self.set_child(k, child); }
        if entry.1 { self.val_mask.set_bit(k); }
    }
    /// Sets the value at `k`, returning the previous one
    fn set_val(&mut self, k: u8, val: V) -> Option<V> {
        core::mem::forget(val);
        let had = self.val_mask.test_bit(k);
        self.val_mask.set_bit(k);
        had.then(zst::<V>)
    }
    #[inline(always)]
    fn make_child<F: FnOnce(&mut crate::line_list_node::LineListNode<V, A>)>(&self, f: F) -> TrieNodeODRc<V, A> {
        let mut child = crate::line_list_node::LineListNode::new_in(self.alloc.clone());
        f(&mut child);
        TrieNodeODRc::new_in(child, self.alloc.clone())
    }
    fn into_node(self) -> TrieNodeODRc<V, A> {
        let alloc = self.alloc.clone();
        TrieNodeODRc::new_in(self, alloc)
    }

    // =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=
    // Entry-level lattice operations, mirroring the CoFree implementations in dense_byte_node.rs
    // with the value part reduced to the (single) zero-sized value
    // =-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=-=

    /// Lattice result of joining/meeting two present values, as an `AlgebraicResult<bool>` where
    /// `Element(true)` means a value is present in the output
    fn val_op(present_l: bool, present_r: bool, both: impl FnOnce() -> AlgebraicResult<V>) -> AlgebraicResult<bool> {
        match (present_l, present_r) {
            (false, false) => AlgebraicResult::None,
            (true, false) => AlgebraicResult::Identity(SELF_IDENT),
            (false, true) => AlgebraicResult::Identity(COUNTER_IDENT),
            (true, true) => both().map(|_| true),
        }
    }
    fn combine(l: Entry<'_, V, A>, r: Entry<'_, V, A>, rec: AlgebraicResult<Option<TrieNodeODRc<V, A>>>, val: AlgebraicResult<bool>) -> AlgebraicResult<OwnedEntry<V, A>> {
        // Same policy as CfShared::combine_algebraic_results in dense_byte_node.rs
        let rec_of = |mask: u64| if mask & SELF_IDENT > 0 { l.0.cloned() } else { r.0.cloned() };
        let val_of = |mask: u64| if mask & SELF_IDENT > 0 { l.1 } else { r.1 };
        match (rec, val) {
            (AlgebraicResult::None, AlgebraicResult::None) => AlgebraicResult::None,
            (AlgebraicResult::Identity(rec_mask), AlgebraicResult::Identity(val_mask)) => {
                let new_mask = rec_mask & val_mask;
                if new_mask > 0 {
                    AlgebraicResult::Identity(new_mask)
                } else {
                    AlgebraicResult::Element((rec_of(rec_mask), val_of(val_mask)))
                }
            },
            (AlgebraicResult::None, AlgebraicResult::Identity(val_mask)) => {
                let mut new_mask = val_mask;
                if l.0.is_some() { new_mask &= !SELF_IDENT; }
                if r.0.is_some() { new_mask &= !COUNTER_IDENT; }
                if new_mask > 0 {
                    AlgebraicResult::Identity(new_mask)
                } else {
                    AlgebraicResult::Element((None, val_of(val_mask)))
                }
            },
            (AlgebraicResult::Identity(rec_mask), AlgebraicResult::None) => {
                let mut new_mask = rec_mask;
                if l.1 { new_mask &= !SELF_IDENT; }
                if r.1 { new_mask &= !COUNTER_IDENT; }
                if new_mask > 0 {
                    AlgebraicResult::Identity(new_mask)
                } else {
                    AlgebraicResult::Element((rec_of(rec_mask), false))
                }
            },
            (rec_el, val_el) => {
                let rec = rec_el.flatten().map_into_option(|arg_idx| if arg_idx == 0 { l.0.cloned() } else { r.0.cloned() });
                let val = val_el.map_into_option(|arg_idx| Some(if arg_idx == 0 { l.1 } else { r.1 })).unwrap_or(false);
                if rec.is_none() && !val {
                    AlgebraicResult::None
                } else {
                    AlgebraicResult::Element((rec, val))
                }
            },
        }
    }
    fn entry_pjoin(l: Entry<'_, V, A>, r: Entry<'_, V, A>) -> AlgebraicResult<OwnedEntry<V, A>> where V: Lattice {
        let rec = l.0.pjoin(&r.0).flatten();
        let val = Self::val_op(l.1, r.1, || zst::<V>().pjoin(&zst::<V>()));
        rec.merge(val, |which| if which == 0 { l.0.cloned() } else { r.0.cloned() }, |which| Some(if which == 0 { l.1 } else { r.1 }), |rec, val| AlgebraicResult::Element((rec, val.unwrap_or(false))))
    }
    fn entry_pmeet(l: Entry<'_, V, A>, r: Entry<'_, V, A>) -> AlgebraicResult<OwnedEntry<V, A>> where V: Lattice {
        let mut identity_flag = 0;
        if l.0.is_none() && !l.1 { identity_flag = SELF_IDENT; }
        if r.0.is_none() && !r.1 { identity_flag |= COUNTER_IDENT; }
        if identity_flag > 0 {
            return AlgebraicResult::Identity(identity_flag)
        }
        let rec = l.0.pmeet(&r.0);
        // Option<V>::pmeet is None unless both values are present
        let val = if l.1 && r.1 { zst::<V>().pmeet(&zst::<V>()).map(|_| true) } else { AlgebraicResult::None };
        Self::combine(l, r, rec, val)
    }
    fn entry_psubtract(l: Entry<'_, V, A>, r: Entry<'_, V, A>) -> AlgebraicResult<OwnedEntry<V, A>> where V: DistributiveLattice {
        let self_rec = l.0.filter(|child| !child.as_tagged().node_is_empty());
        let rec = self_rec.psubtract(&r.0);
        let val = match (l.1, r.1) {
            (false, _) => AlgebraicResult::None,
            (true, false) => AlgebraicResult::Identity(SELF_IDENT),
            (true, true) => zst::<V>().psubtract(&zst::<V>()).map(|_| true),
        };
        Self::combine(l, r, rec, val)
    }
    fn entry_prestrict(l: Entry<'_, V, A>, r: Entry<'_, V, A>) -> AlgebraicResult<OwnedEntry<V, A>> {
        if r.1 { return AlgebraicResult::Identity(SELF_IDENT) }
        match (l.0, r.0) {
            (Some(lc), Some(rc)) => match lc.prestrict(rc) {
                AlgebraicResult::Identity(mask) => {
                    debug_assert_eq!(mask, SELF_IDENT);
                    if l.1 { AlgebraicResult::Element((Some(lc.clone()), false)) } else { AlgebraicResult::Identity(SELF_IDENT) }
                },
                AlgebraicResult::None => AlgebraicResult::None,
                AlgebraicResult::Element(node) => AlgebraicResult::Element((Some(node), false)),
            },
            _ => AlgebraicResult::None,
        }
    }

    /// Joins two unit byte nodes, see `ByteNode::pjoin`
    fn pjoin_unit(&self, other: &Self) -> AlgebraicResult<Self> where V: Lattice {
        let sm = self.entry_mask();
        let om = other.entry_mask();
        let jm = sm | om;
        let mm = sm & om;
        let mut is_identity = sm == jm;
        let mut is_counter_identity = om == jm;
        let mut new = Self::new_in(self.alloc.clone());
        new.children.reserve_exact(jm.count_bits());
        for k in jm.iter() {
            if mm.test_bit(k) {
                match Self::entry_pjoin(self.entry(k), other.entry(k)) {
                    AlgebraicResult::None => unreachable!(),
                    AlgebraicResult::Identity(mask) => {
                        if mask & SELF_IDENT == 0 { is_identity = false; }
                        if mask & COUNTER_IDENT == 0 { is_counter_identity = false; }
                        let src = if mask & SELF_IDENT > 0 { self.entry(k) } else { other.entry(k) };
                        new.push_entry(k, (src.0.cloned(), src.1));
                    },
                    AlgebraicResult::Element(e) => {
                        is_identity = false;
                        is_counter_identity = false;
                        new.push_entry(k, e);
                    },
                }
            } else if sm.test_bit(k) {
                is_counter_identity = false;
                let e = self.entry(k);
                new.push_entry(k, (e.0.cloned(), e.1));
            } else {
                is_identity = false;
                let e = other.entry(k);
                new.push_entry(k, (e.0.cloned(), e.1));
            }
        }
        if new.entry_mask().is_empty_mask() {
            AlgebraicResult::None
        } else if is_identity || is_counter_identity {
            let mut mask = 0;
            if is_identity { mask |= SELF_IDENT; }
            if is_counter_identity { mask |= COUNTER_IDENT; }
            AlgebraicResult::Identity(mask)
        } else {
            AlgebraicResult::Element(new)
        }
    }
    /// Joins `other` into `self`, see `ByteNode::join_into`
    fn join_into_unit(&mut self, mut other: Self) -> AlgebraicStatus where V: Lattice {
        let sm = self.entry_mask();
        let om = other.entry_mask();
        let jm = sm | om;
        let mut is_identity = sm == jm;
        let mut new = Self::new_in(self.alloc.clone());
        new.children.reserve_exact(jm.count_bits());
        for k in jm.iter() {
            let self_child = self.take_child(k);
            let self_val = self.val_mask.test_bit(k);
            let other_child = other.take_child(k);
            let other_val = other.val_mask.test_bit(k);
            if !sm.test_bit(k) {
                is_identity = false;
                new.push_entry(k, (other_child, other_val));
                continue;
            }
            let child = match (self_child, other_child) {
                (Some(mut sc), Some(oc)) => {
                    let (status, result) = sc.make_mut().join_into_dyn(oc);
                    if let Err(replacement) = result { sc = replacement; }
                    if status.is_element() { is_identity = false; }
                    Some(sc)
                },
                (Some(sc), None) => Some(sc),
                (None, Some(oc)) => { is_identity = false; Some(oc) },
                (None, None) => None,
            };
            let val = match (self_val, other_val) {
                (true, true) => { let mut v = zst::<V>(); if v.join_into(zst::<V>()).is_element() { is_identity = false; } core::mem::forget(v); true },
                (true, false) => true,
                (false, true) => { is_identity = false; true },
                (false, false) => false,
            };
            new.push_entry(k, (child, val));
        }
        *self = new;
        if self.entry_mask().is_empty_mask() {
            AlgebraicStatus::None
        } else if is_identity {
            AlgebraicStatus::Identity
        } else {
            AlgebraicStatus::Element
        }
    }
    /// Generic intersection-style operation (meet, restrict) over the common entries
    fn common_op(&self, other: &Self, commutative: bool, f: impl Fn(Entry<'_, V, A>, Entry<'_, V, A>) -> AlgebraicResult<OwnedEntry<V, A>>) -> AlgebraicResult<Self> {
        let sm = self.entry_mask();
        let om = other.entry_mask();
        let mm = sm & om;
        let mut is_identity = sm == mm && (commutative || om == mm);
        let mut is_counter_identity = commutative && om == mm;
        let mut new = Self::new_in(self.alloc.clone());
        for k in mm.iter() {
            match f(self.entry(k), other.entry(k)) {
                AlgebraicResult::None => {
                    is_identity = false;
                    is_counter_identity = false;
                },
                AlgebraicResult::Identity(mask) => {
                    if mask & SELF_IDENT == 0 { is_identity = false; }
                    if mask & COUNTER_IDENT == 0 { is_counter_identity = false; }
                    let src = if mask & SELF_IDENT > 0 { self.entry(k) } else { other.entry(k) };
                    new.push_entry(k, (src.0.cloned(), src.1));
                },
                AlgebraicResult::Element(e) => {
                    is_identity = false;
                    is_counter_identity = false;
                    new.push_entry(k, e);
                },
            }
        }
        if new.entry_mask().is_empty_mask() {
            AlgebraicResult::None
        } else if is_identity || is_counter_identity {
            let mut mask = 0;
            if is_identity { mask |= SELF_IDENT; }
            if is_counter_identity { mask |= COUNTER_IDENT; }
            AlgebraicResult::Identity(mask)
        } else {
            AlgebraicResult::Element(new)
        }
    }
    fn psubtract_unit(&self, other: &Self) -> AlgebraicResult<Self> where V: DistributiveLattice {
        let mut is_identity = true;
        let mut new = self.clone();
        for k in (self.entry_mask() & other.entry_mask()).iter() {
            match Self::entry_psubtract(self.entry(k), other.entry(k)) {
                AlgebraicResult::None => { is_identity = false; new.remove_entry(k); },
                AlgebraicResult::Identity(mask) => { debug_assert_eq!(mask, SELF_IDENT); },
                AlgebraicResult::Element(e) => { is_identity = false; new.remove_entry(k); new.push_entry(k, e); },
            }
        }
        if new.entry_mask().is_empty_mask() {
            AlgebraicResult::None
        } else if is_identity {
            AlgebraicResult::Identity(SELF_IDENT)
        } else {
            AlgebraicResult::Element(new)
        }
    }
}

impl<V: Clone + Send + Sync, A: Allocator> TrieNode<V, A> for UnitByteNode<V, A> {
    #[inline(always)]
    fn node_key_overlap(&self, key: &[u8]) -> usize {
        self.has_entry(key[0]) as usize
    }
    #[inline(always)]
    fn node_contains_partial_key(&self, key: &[u8]) -> bool {
        debug_assert!(key.len() > 0);
        key.len() == 1 && self.has_entry(key[0])
    }
    #[inline(always)]
    fn node_get_child(&self, key: &[u8]) -> Option<(usize, &TrieNodeODRc<V, A>)> {
        self.child(key[0]).map(|child| (1, child))
    }
    fn node_get_child_mut(&mut self, key: &[u8]) -> Option<(usize, &mut TrieNodeODRc<V, A>)> {
        debug_assert!(key.len() > 0);
        self.child_mut(key[0]).map(|child| (1, child))
    }
    fn node_replace_child(&mut self, key: &[u8], new_node: TrieNodeODRc<V, A>) {
        debug_assert!(key.len() == 1);
        *self.child_mut(key[0]).unwrap() = new_node;
    }
    fn node_get_payloads<'node, 'res>(&'node self, keys: &[(&[u8], bool)], results: &'res mut [(usize, PayloadRef<'node, V, A>)]) -> bool {
        // Same algorithm as ByteNode::node_get_payloads
        let mut unrequested_cofree_half = false;
        let mut stashed_val: Option<&V> = None;
        let mut last_byte: Option<u8> = None;
        let mut requested_mask = self.entry_mask();

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
                requested_mask.clear_bit(byte);
                if self.has_entry(byte) {
                    let (rec, has_val) = self.entry(byte);
                    if key.len() == 1 && *expect_val && rec.is_some() {
                        unrequested_cofree_half = true;
                    }
                    if key.len() > 1 || !*expect_val {
                        if let Some(rec) = rec {
                            *result_key_len = 1;
                            *payload_ref = PayloadRef::Child(rec);
                        }
                    }
                    if has_val {
                        let val = zst_ref::<V>();
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
        !unrequested_cofree_half && stashed_val.is_none() && requested_mask.is_empty_mask()
    }
    fn node_contains_val(&self, key: &[u8]) -> bool {
        key.len() == 1 && self.val_mask.test_bit(key[0])
    }
    fn node_get_val(&self, key: &[u8]) -> Option<&V> {
        if self.node_contains_val(key) { Some(zst_ref::<V>()) } else { None }
    }
    fn node_get_val_mut(&mut self, key: &[u8]) -> Option<&mut V> {
        if self.node_contains_val(key) { Some(zst_mut::<V>()) } else { None }
    }
    fn node_set_val(&mut self, key: &[u8], val: V) -> Result<(Option<V>, bool), TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        if key.len() > 1 {
            let child = self.make_child(|child| { child.node_set_val(&key[1..], val).unwrap_or_else(|_| panic!()); });
            self.set_child(key[0], child);
            Ok((None, true))
        } else {
            Ok((self.set_val(key[0], val), false))
        }
    }
    fn node_remove_val(&mut self, key: &[u8], prune: bool) -> Option<V> {
        if key.len() != 1 { return None; }
        let k = key[0];
        if !self.val_mask.test_bit(k) { return None; }
        self.val_mask.clear_bit(k);
        if prune && !self.child_mask.test_bit(k) {
            // nothing else to remove: the entry is gone with its value bit
        } else if !prune && !self.child_mask.test_bit(k) {
            // Keep the path as a dangling entry, as the dense node keeps an empty cofree
            self.set_child(k, TrieNodeODRc::new_empty());
        }
        Some(zst::<V>())
    }
    fn node_create_dangling(&mut self, key: &[u8]) -> Result<(bool, bool), TrieNodeODRc<V, A>> {
        debug_assert!(key.len() > 0);
        if key.len() > 1 {
            let child = self.make_child(|child| { child.node_create_dangling(&key[1..]).unwrap_or_else(|_| panic!()); });
            self.set_child(key[0], child);
            Ok((true, true))
        } else if self.has_entry(key[0]) {
            Ok((false, false))
        } else {
            self.set_child(key[0], TrieNodeODRc::new_empty());
            Ok((true, false))
        }
    }
    fn node_remove_dangling(&mut self, key: &[u8]) -> usize {
        debug_assert!(key.len() > 0);
        if key.len() == 1 {
            let k = key[0];
            if let Some(child) = self.child(k) {
                if child.as_tagged().node_is_empty() {
                    if self.val_mask.test_bit(k) {
                        drop(self.take_child(k));
                    } else {
                        self.remove_entry(k);
                        return 1
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
            self.set_child(key[0], child);
            Ok(true)
        } else {
            self.set_child(key[0], new_node);
            Ok(false)
        }
    }
    fn node_remove_all_branches(&mut self, key: &[u8], prune: bool) -> bool {
        if key.len() > 1 { return false; }
        debug_assert_eq!(key.len(), 1);
        let k = key[0];
        match (self.child_mask.test_bit(k), self.val_mask.test_bit(k)) {
            (true, true) => { drop(self.take_child(k)); true },
            (true, false) => {
                if prune { self.remove_entry(k); } else { *self.child_mut(k).unwrap() = TrieNodeODRc::new_empty(); }
                true
            },
            (false, _) => false,
        }
    }
    fn node_remove_unmasked_branches(&mut self, key: &[u8], mask: ByteMask, _prune: bool) {
        if key.len() > 0 { return }
        let keep = self.child_mask & mask;
        if keep != self.child_mask {
            let mut kept = Vec::with_capacity(keep.count_bits());
            let children = core::mem::take(&mut self.children);
            for (k, child) in self.child_mask.iter().zip(children.into_iter()) {
                if mask.test_bit(k) { kept.push(child); }
            }
            self.children = kept;
            self.child_mask = keep;
        }
        self.val_mask = self.val_mask & mask;
    }
    fn node_is_empty(&self) -> bool {
        self.child_mask.is_empty_mask() && self.val_mask.is_empty_mask()
    }
    // Iter tokens: the byte at which to resume scanning (0..=256)
    #[inline(always)]
    fn new_iter_token(&self) -> IterToken { 0 }
    #[inline(always)]
    fn iter_token_for_path(&self, key: &[u8]) -> IterToken {
        if key.len() != 1 { 0 } else { key[0] as IterToken + 1 }
    }
    #[inline(always)]
    fn next_items(&self, token: IterToken) -> (IterToken, &[u8], Option<&TrieNodeODRc<V, A>>, Option<&V>) {
        let em = self.entry_mask();
        let next = if token == 0 { em.indexed_bit::<true>(0) } else if token > 255 { None } else { em.next_bit((token - 1) as u8) };
        match next {
            Some(k) => {
                let (child, has_val) = self.entry(k);
                (k as IterToken + 1, &ALL_BYTES[k as usize..=k as usize], child, has_val.then(zst_ref::<V>))
            },
            None => (NODE_ITER_FINISHED, &[], None, None),
        }
    }
    fn node_val_count(&self, cache: &mut HashMap<u64, usize>) -> usize {
        self.val_mask.count_bits() + self.children.iter().map(|r| val_count_below_node(r, cache)).sum::<usize>()
    }
    fn node_goat_val_count(&self) -> usize {
        self.val_mask.count_bits()
    }
    fn node_child_iter_start(&self) -> (u64, Option<&TrieNodeODRc<V, A>>) {
        self.node_child_iter_next(0)
    }
    fn node_child_iter_next(&self, token: u64) -> (u64, Option<&TrieNodeODRc<V, A>>) {
        match self.children.get(token as usize) {
            Some(child) => (token + 1, Some(child)),
            None => (0, None),
        }
    }
    #[cfg(feature = "counters")]
    fn item_count(&self) -> usize {
        self.children.len() + self.val_mask.count_bits()
    }
    fn node_first_val_depth_along_key(&self, key: &[u8]) -> Option<usize> {
        debug_assert!(key.len() > 0);
        if self.val_mask.test_bit(key[0]) { Some(0) } else { None }
    }
    fn nth_child_from_key(&self, key: &[u8], n: usize) -> (Option<u8>, Option<TaggedNodeRef<'_, V, A>>) {
        if key.len() > 0 {
            return (None, None)
        }
        match self.entry_mask().indexed_bit::<true>(n) {
            Some(k) => (Some(k), self.child(k).map(|c| c.as_tagged())),
            None => (None, None),
        }
    }
    fn first_child_from_key(&self, key: &[u8]) -> (Option<&[u8]>, Option<TaggedNodeRef<'_, V, A>>) {
        debug_assert_eq!(key.len(), 0);
        let k = self.entry_mask().indexed_bit::<true>(0).unwrap();
        (Some(&ALL_BYTES[k as usize..=k as usize]), self.child(k).map(|c| c.as_tagged()))
    }
    #[inline(always)]
    fn node_branches_mask(&self, key: &[u8]) -> ByteMask {
        match key.len() {
            0 => self.entry_mask(),
            _ => ByteMask::EMPTY,
        }
    }
    #[inline(always)]
    fn count_branches(&self, key: &[u8]) -> usize {
        match key.len() {
            0 => self.entry_count(),
            _ => 0,
        }
    }
    fn prior_branch_key<'key>(&self, key: &'key [u8]) -> &'key [u8] {
        debug_assert!(key.len() >= 1);
        if key.len() > 1 && self.has_entry(key[0]) { &key[0..1] } else { &[] }
    }
    fn get_sibling_of_child(&self, key: &[u8], next: bool) -> (Option<u8>, Option<TaggedNodeRef<'_, V, A>>) {
        if key.len() != 1 {
            return (None, None)
        }
        let em = self.entry_mask();
        let sibling = if next { em.next_bit(key[0]) } else { em.prev_bit(key[0]) };
        match sibling {
            Some(k) => (Some(k), self.child(k).map(|c| c.as_tagged())),
            None => (None, None),
        }
    }
    fn get_node_at_key(&self, key: &[u8]) -> AbstractNodeRef<'_, V, A> {
        match key.len() {
            0 => if !self.node_is_empty() { AbstractNodeRef::BorrowedDyn(self.as_tagged()) } else { AbstractNodeRef::None },
            1 => match self.child(key[0]) {
                Some(link) => AbstractNodeRef::BorrowedRc(link),
                None => AbstractNodeRef::None,
            },
            _ => AbstractNodeRef::None,
        }
    }
    fn take_node_at_key(&mut self, key: &[u8], prune: bool) -> Option<TrieNodeODRc<V, A>> {
        if key.len() != 1 { return None; }
        let k = key[0];
        let result = self.take_child(k);
        if !prune && result.is_some() && !self.val_mask.test_bit(k) {
            self.set_child(k, TrieNodeODRc::new_empty());
        }
        result
    }
    fn pjoin_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: Lattice {
        match other {
            TaggedNodeRef::UnitByteNode(other) => self.pjoin_unit(other).map(|node| node.into_node()),
            TaggedNodeRef::EmptyNode => AlgebraicResult::Identity(SELF_IDENT),
            _ => self.to_dense().pjoin_dyn(other),
        }
    }
    fn join_into_dyn(&mut self, mut other: TrieNodeODRc<V, A>) -> (AlgebraicStatus, Result<(), TrieNodeODRc<V, A>>) where V: Lattice {
        if other.as_tagged().node_is_empty() {
            return (AlgebraicStatus::Identity, Ok(()));
        }
        if let TaggedNodeRefMut::UnitByteNode(other_unit) = other.make_mut() {
            let taken = core::mem::replace(other_unit, Self::new_in(self.alloc.clone()));
            return (self.join_into_unit(taken), Ok(()));
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
        match other {
            TaggedNodeRef::UnitByteNode(other) => self.common_op(other, true, Self::entry_pmeet).map(|node| node.into_node()),
            TaggedNodeRef::EmptyNode => AlgebraicResult::None,
            _ => self.to_dense().pmeet_dyn(other),
        }
    }
    fn psubtract_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> where V: DistributiveLattice {
        match other {
            TaggedNodeRef::UnitByteNode(other) => self.psubtract_unit(other).map(|node| node.into_node()),
            TaggedNodeRef::EmptyNode => AlgebraicResult::Identity(SELF_IDENT),
            _ => self.to_dense().psubtract_dyn(other),
        }
    }
    fn prestrict_dyn(&self, other: TaggedNodeRef<V, A>) -> AlgebraicResult<TrieNodeODRc<V, A>> {
        match other {
            TaggedNodeRef::UnitByteNode(other) => self.common_op(other, false, Self::entry_prestrict).map(|node| node.into_node()),
            TaggedNodeRef::EmptyNode => AlgebraicResult::None,
            _ => self.to_dense().prestrict_dyn(other),
        }
    }
    fn clone_self(&self) -> TrieNodeODRc<V, A> {
        TrieNodeODRc::new_in(self.clone(), self.alloc.clone())
    }
}

impl<V: Clone + Send + Sync, A: Allocator> TrieNodeDowncast<V, A> for UnitByteNode<V, A> {
    #[inline]
    fn tag(&self) -> usize {
        UNIT_BYTE_NODE_TAG
    }
    #[inline]
    fn as_tagged(&self) -> TaggedNodeRef<'_, V, A> {
        TaggedNodeRef::UnitByteNode(self)
    }
    #[cfg(not(feature="slim_ptrs"))]
    fn as_tagged_mut(&mut self) -> TaggedNodeRefMut<'_, V, A> {
        TaggedNodeRefMut::UnitByteNode(self)
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

    fn tag_counts<V: Clone + Send + Sync + Unpin>(map: &PathMap<V>) -> HashMap<usize, usize> {
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

    fn paths<V: Clone + Send + Sync + Unpin>(map: &PathMap<V>) -> Vec<Vec<u8>> {
        let mut out = vec![];
        let mut rz = map.read_zipper();
        while rz.to_next_val() { out.push(rz.path().to_vec()); }
        out
    }

    #[test]
    fn seven_entries_make_a_unit_byte_node() {
        let mut map: PathMap<()> = PathMap::new();
        for b in 0..7u8 {
            map.set_val_at([b'a', b], ());
        }
        let counts = tag_counts(&map);
        assert_eq!(counts.get(&UNIT_BYTE_NODE_TAG), Some(&1), "{counts:?}");
        assert_eq!(counts.get(&DENSE_BYTE_NODE_TAG), None, "{counts:?}");
        for b in 0..=255u8 {
            map.set_val_at([b'a', b], ());
        }
        assert_eq!(map.val_count(), 256);
        assert_eq!(tag_counts(&map).get(&UNIT_BYTE_NODE_TAG), Some(&1));
        assert_eq!(paths(&map).len(), 256);
        // Values with values below them, dangling paths, removals
        map.set_val_at(b"a\x05/deeper", ());
        assert!(map.contains(b"a\x05") && map.contains(b"a\x05/deeper"));
        assert_eq!(map.remove_val_at(b"a\x05", true), Some(()));
        assert!(!map.contains(b"a\x05") && map.contains(b"a\x05/deeper"));
        assert_eq!(map.remove_val_at(b"a\x06", true), Some(()));
        assert!(!map.path_exists_at(b"a\x06"));
        let mut wz = map.write_zipper_at_path(b"a\x07");
        assert_eq!(wz.remove_val(false), Some(()));
        assert!(wz.path_exists() && !wz.is_val());
        wz.set_val(());
        drop(wz);
        let mut wz = map.write_zipper_at_path(b"b");
        assert!(wz.create_path());
        assert!(wz.path_exists());
        assert!(wz.prune_path() > 0);
        drop(wz);
        assert_eq!(map.val_count(), 255);
        // A non-unit value type never produces unit byte nodes
        let mut map: PathMap<u32> = PathMap::new();
        for b in 0..20u8 { map.set_val_at([b], b as u32); }
        assert_eq!(tag_counts(&map).get(&UNIT_BYTE_NODE_TAG), None);
        assert_eq!(tag_counts(&map).get(&DENSE_BYTE_NODE_TAG), Some(&1));
    }

    /// Runs the set operations on unit tries and on identical u64 tries (which use dense nodes)
    /// and checks that the resulting key sets agree
    #[test]
    fn unit_algebra_matches_dense_algebra() {
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut next = || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
        for round in 0..40 {
            let make = |n: usize, next: &mut dyn FnMut() -> u64| -> Vec<Vec<u8>> {
                (0..n).map(|_| {
                    let r = next();
                    let len = 1 + (r % 4) as usize;
                    let alphabet = 3 + (round % 12) as u64;
                    (0..len).map(|i| b'a' + ((r >> (8 * i)) % alphabet) as u8).collect()
                }).collect()
            };
            let (ka, kb) = (make(30 + round * 3, &mut next), make(30 + round * 2, &mut next));
            let mut ua: PathMap<()> = PathMap::new();
            let mut ub: PathMap<()> = PathMap::new();
            let mut da: PathMap<u64> = PathMap::new();
            let mut db: PathMap<u64> = PathMap::new();
            for k in &ka { ua.set_val_at(k, ()); da.set_val_at(k, 1); }
            for k in &kb { ub.set_val_at(k, ()); db.set_val_at(k, 1); }
            assert_eq!(paths(&ua), paths(&da));
            assert_eq!(paths(&ua.join(&ub)), paths(&da.join(&db)), "join round {round}");
            assert_eq!(paths(&ua.meet(&ub)), paths(&da.meet(&db)), "meet round {round}");
            assert_eq!(paths(&ua.subtract(&ub)), paths(&da.subtract(&db)), "subtract round {round}");
            assert_eq!(paths(&ua.restrict(&ub)), paths(&da.restrict(&db)), "restrict round {round}");
            let mut ua2 = ua.clone();
            ua2.write_zipper().join_into(&ub.read_zipper());
            let mut da2 = da.clone();
            da2.write_zipper().join_into(&db.read_zipper());
            assert_eq!(paths(&ua2), paths(&da2), "join_into round {round}");
            // Joining a trie with itself is an identity, and with its clone as well
            assert_eq!(paths(&ua.join(&ua)), paths(&ua));
            let mut ua3 = ua.clone();
            assert!(ua3.write_zipper().join_into(&ua.read_zipper()).is_identity() || ua.val_count() == 0);
            let mut ua4 = ua.clone();
            ua4.write_zipper().meet_into(&ub.read_zipper(), true);
            assert_eq!(paths(&ua4), paths(&ua.meet(&ub)), "meet_into round {round}");
            let mut ua5 = ua.clone();
            ua5.write_zipper().subtract_into(&ub.read_zipper(), true);
            assert_eq!(paths(&ua5), paths(&ua.subtract(&ub)), "subtract_into round {round}");
        }
    }
}

