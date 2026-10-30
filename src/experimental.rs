#![allow(warnings)] 

use crate::alloc::Allocator;
use crate::utils::ByteMask;
use crate::PathMap;
use crate::trie_node::*;
use crate::zipper::*;
use crate::ring::{AlgebraicStatus, DistributiveLattice, Lattice};
use crate::TrieValue;
use crate::write_zipper::write_zipper_priv::WriteZipperPriv;

#[cfg(feature = "serialization")]
pub mod serialization;

#[cfg(feature = "zipper_alg")]
pub mod zipper_algebra;
#[cfg(not(feature = "zipper_alg"))]
mod zipper_algebra;

struct FullZipper {
    /// Identifies the zipper in traces, see [crate::trace]
    uid: crate::trace::ZipperUid,
    path: Vec<u8>
}

impl Zipper for FullZipper {
    fn trace_uid(&self) -> u64 { self.uid.get() }
    fn path_exists(&self) -> bool { crate::trace::trace_point!(path_exists, self.uid); true }
    fn is_val(&self) -> bool { crate::trace::trace_point!(is_val, self.uid); true }
    fn child_count(&self) -> usize { crate::trace::trace_point!(child_count, self.uid); 256 }
    fn child_mask(&self) -> ByteMask { crate::trace::trace_point!(child_mask, self.uid); [!0u64, !0u64, !0u64, !0u64].into() }
}

impl ZipperPathBuffer for FullZipper {
    unsafe fn origin_path_assert_len(&self, len: usize) -> &[u8] {
        assert!(len <= self.path.capacity());
        unsafe{ core::slice::from_raw_parts(self.path.as_ptr(), len) }
    }

    fn prepare_buffers(&mut self) {
        self.reserve_buffers(EXPECTED_PATH_LEN, 0)
    }
    fn reserve_buffers(&mut self, path_len: usize, _stack_depth: usize) {
        self.path.reserve(path_len)
    }
}

impl ZipperMoving for FullZipper {
    #[inline] fn depth(&self) -> usize { crate::trace::trace_point!(depth, self.uid); self.path.len() }
    #[inline]
    fn focus_byte(&self) -> Option<u8> { crate::trace::trace_point!(focus_byte, self.uid); self.path.last().cloned() }
    fn reset(&mut self) { crate::trace::trace_point!(reset, self.uid); self.path.clear() }
    fn val_count(&self) -> usize { crate::trace::trace_point!(val_count, self.uid); usize::MAX/2 } // usize::MAX is a dangerous default for overflow
    fn descend_to<K: AsRef<[u8]>>(&mut self, k: K) {
        crate::trace::trace_point!(descend_to, self.uid, AsRef::<[u8]>::as_ref(&k).as_ptr(), AsRef::<[u8]>::as_ref(&k).len());
        self.path.extend_from_slice(k.as_ref());
    }
    fn descend_to_byte(&mut self, k: u8) {
        crate::trace::trace_point!(descend_to_byte, self.uid, k);
        self.path.push(k);
    }
    fn descend_indexed_byte(&mut self, idx: usize) -> Option<u8> {
        crate::trace::trace_point!(descend_indexed_byte, self.uid, idx);
        assert!(idx < 256);
        self.path.push(idx as u8);
        Some(idx as u8)
    }
    fn descend_first_byte(&mut self) -> Option<u8> {
        crate::trace::trace_point!(descend_first_byte, self.uid);
        self.path.push(0);
        Some(0)
    }
    fn descend_until_observed<Obs: PathObserver>(&mut self, obs: &mut Obs) -> bool {
        crate::trace::trace_point!(descend_until_observed, self.uid);
        self.path.push(0); // not sure?
        obs.descend_to_byte(0);
        true
    }
    fn ascend(&mut self, steps: usize) -> usize {
        crate::trace::trace_point!(ascend, self.uid, steps);
        let ascended = steps.min(self.path.len());
        self.path.truncate(self.path.len() - ascended);
        ascended
    }
    fn ascend_byte(&mut self) -> bool {
        crate::trace::trace_point!(ascend_byte, self.uid);
        self.path.pop().is_some()
    }
    fn ascend_until(&mut self) -> usize {
        crate::trace::trace_point!(ascend_until, self.uid);
        if self.path.pop().is_some() { 1 } else { 0 } // not sure?
    }
    fn ascend_until_branch(&mut self) -> usize {
        crate::trace::trace_point!(ascend_until_branch, self.uid);
        if self.path.pop().is_some() { 1 } else { 0 } // not sure? What's the difference with the previous?
    }
    fn to_next_sibling_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(to_next_sibling_byte, self.uid); self.to_sibling(true) }
    fn to_prev_sibling_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(to_prev_sibling_byte, self.uid); self.to_sibling(false) }
}

impl ZipperPath for FullZipper {
    fn path(&self) -> &[u8] { crate::trace::trace_point!(path, self.uid); &self.path[..] }
}

impl FullZipper {
    fn to_sibling(&mut self, next: bool) -> Option<u8> {
        if self.path.is_empty() { return None } // right?
        if next {
            let last = self.path.last_mut().unwrap();
            if *last != 255 { *last = *last + 1; Some(*last) }
            else { None }
        } else {
            let first = self.path.first_mut().unwrap();
            if *first != 0 { *first = *first - 1; Some(*first) }
            else { None }
        }
    }
}

// Doesn't seem as lawful as the above, still maybe useful for testing
struct NullZipper {
    /// Identifies the zipper in traces, see [crate::trace]
    uid: crate::trace::ZipperUid,}

impl Zipper for NullZipper {
    fn trace_uid(&self) -> u64 { self.uid.get() }
    fn path_exists(&self) -> bool { crate::trace::trace_point!(path_exists, self.uid); false }
    fn is_val(&self) -> bool { crate::trace::trace_point!(is_val, self.uid); false }
    fn child_count(&self) -> usize { crate::trace::trace_point!(child_count, self.uid); 0 }
    fn child_mask(&self) -> ByteMask { crate::trace::trace_point!(child_mask, self.uid); ByteMask::EMPTY }
}

impl ZipperMoving for NullZipper {
    #[inline] fn depth(&self) -> usize { crate::trace::trace_point!(depth, self.uid); 0 }
    #[inline]
    fn focus_byte(&self) -> Option<u8> { crate::trace::trace_point!(focus_byte, self.uid); None }
    fn reset(&mut self) { crate::trace::trace_point!(reset, self.uid);}
    fn val_count(&self) -> usize { crate::trace::trace_point!(val_count, self.uid); 0 }
    fn descend_to<K: AsRef<[u8]>>(&mut self, _k: K) { crate::trace::trace_point!(descend_to, self.uid, AsRef::<[u8]>::as_ref(&_k).as_ptr(), AsRef::<[u8]>::as_ref(&_k).len());}
    fn descend_to_byte(&mut self, _k: u8) { crate::trace::trace_point!(descend_to_byte, self.uid, _k);}
    fn descend_indexed_byte(&mut self, _idx: usize) -> Option<u8> { crate::trace::trace_point!(descend_indexed_byte, self.uid, _idx); None }
    fn descend_first_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(descend_first_byte, self.uid); None }
    fn descend_until_observed<Obs: PathObserver>(&mut self, _obs: &mut Obs) -> bool { crate::trace::trace_point!(descend_until_observed, self.uid); false }
    fn ascend(&mut self, _steps: usize) -> usize { crate::trace::trace_point!(ascend, self.uid, _steps); 0 }
    fn ascend_byte(&mut self) -> bool { crate::trace::trace_point!(ascend_byte, self.uid); false }
    fn ascend_until(&mut self) -> usize { crate::trace::trace_point!(ascend_until, self.uid); 0 }
    fn ascend_until_branch(&mut self) -> usize { crate::trace::trace_point!(ascend_until_branch, self.uid); 0 }
    fn to_next_sibling_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(to_next_sibling_byte, self.uid); None }
    fn to_prev_sibling_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(to_prev_sibling_byte, self.uid); None }
}

impl ZipperPath for NullZipper {
    fn path(&self) -> &[u8] { crate::trace::trace_point!(path, self.uid); &[] }
}

impl<V: TrieValue, A: Allocator> WriteZipperPriv<V, A> for NullZipper {
    fn take_focus(&mut self, prune: bool) -> Option<TrieNodeODRc<V, A>> {
        None
    }
    fn take_root_prefix_path(&mut self) -> Vec<u8> {
        unimplemented!()
    }
    fn alloc(&self) -> A {
        unimplemented!()
    }
}

impl <V: TrieValue + 'static, A: Allocator> ZipperWriting<V, A> for NullZipper {
    type ZipperHead<'z> = ZipperHead<'z, 'static, V> where Self: 'z;

    fn get_val_mut(&mut self) -> Option<&mut V> { crate::trace::trace_point!(get_val_mut, self.uid); None }
    fn get_val_or_set_mut(&mut self, default: V) -> &mut V { crate::trace::trace_point!(get_val_or_set_mut, self.uid, core::ptr::from_ref(&default)); Box::leak(Box::new(default)) }
    fn get_val_or_set_mut_with<F>(&mut self, func: F) -> &mut V where F: FnOnce() -> V { crate::trace::trace_point!(get_val_or_set_mut_with, self.uid); Box::leak(Box::new(func())) }
    fn set_val(&mut self, _val: V) -> Option<V> { crate::trace::trace_point!(set_val, self.uid, core::ptr::from_ref(&_val)); None }
    fn set_val_at<K: AsRef<[u8]>>(&mut self, path: K, val: V) -> Option<V> { crate::trace::trace_point!(set_val_at, self.uid, AsRef::<[u8]>::as_ref(&path).as_ptr(), AsRef::<[u8]>::as_ref(&path).len(), core::ptr::from_ref(&val)); None }
    fn remove_val(&mut self, _prune: bool) -> Option<V> { crate::trace::trace_point!(remove_val, self.uid, _prune); None }
    fn zipper_head<'z>(&'z mut self) -> Self::ZipperHead<'z> { crate::trace::trace_point!(zipper_head, self.uid); todo!() }
    fn graft<Z: ZipperInfallibleSubtries<V, A>>(&mut self, _read_zipper: &Z) { crate::trace::trace_point!(graft, self.uid, crate::zipper::Zipper::trace_uid(&*_read_zipper));}
    fn graft_src_at<Z: ZipperInfallibleSubtries<V, A>, K: AsRef<[u8]>>(&mut self, src: &Z, path: K) { crate::trace::trace_point!(graft_src_at, self.uid, crate::zipper::Zipper::trace_uid(&*src), AsRef::<[u8]>::as_ref(&path).as_ptr(), AsRef::<[u8]>::as_ref(&path).len());}
    fn graft_map(&mut self, _map: PathMap<V, A>) { crate::trace::trace_point!(graft_map, self.uid);}
    fn join_into<Z: ZipperInfallibleSubtries<V, A>>(&mut self, _read_zipper: &Z) -> AlgebraicStatus where V: Lattice { crate::trace::trace_point!(join_into, self.uid, crate::zipper::Zipper::trace_uid(&*_read_zipper)); AlgebraicStatus::Element }
    fn join_map_into(&mut self, _map: PathMap<V, A>) -> AlgebraicStatus where V: Lattice { crate::trace::trace_point!(join_map_into, self.uid); AlgebraicStatus::Element }
    fn join_into_take<Z: ZipperInfallibleSubtries<V, A> + ZipperWriting<V, A>>(&mut self, _src_zipper: &mut Z, prune: bool) -> AlgebraicStatus where V: Lattice { crate::trace::trace_point!(join_into_take, self.uid, crate::zipper::Zipper::trace_uid(&*_src_zipper), prune); AlgebraicStatus::Element }
    fn join_k_path_into(&mut self, _byte_cnt: usize, _prune: bool) -> bool where V: Lattice { crate::trace::trace_point!(join_k_path_into, self.uid, _byte_cnt, _prune); false }
    fn meet_k_path_into(&mut self, byte_cnt: usize, prune: bool) -> bool where V: Lattice { crate::trace::trace_point!(meet_k_path_into, self.uid, byte_cnt, prune); false }
    fn insert_prefix<K: AsRef<[u8]>>(&mut self, _prefix: K) -> bool { crate::trace::trace_point!(insert_prefix, self.uid, AsRef::<[u8]>::as_ref(&_prefix).as_ptr(), AsRef::<[u8]>::as_ref(&_prefix).len()); false }
    fn remove_prefix(&mut self, _n: usize) -> bool { crate::trace::trace_point!(remove_prefix, self.uid, _n); false }
    fn meet_into<Z: ZipperInfallibleSubtries<V, A>>(&mut self, _read_zipper: &Z, _prune: bool) -> AlgebraicStatus where V: Lattice { crate::trace::trace_point!(meet_into, self.uid, crate::zipper::Zipper::trace_uid(&*_read_zipper), _prune); AlgebraicStatus::Element }
    fn meet_2<'z, ZA: ZipperInfallibleSubtries<V, A>, ZB: ZipperInfallibleSubtries<V, A>>(&mut self, _rz_a: &ZA, _rz_b: &ZB) -> AlgebraicStatus where V: Lattice { crate::trace::trace_point!(meet_2, self.uid, crate::zipper::Zipper::trace_uid(&*_rz_a), crate::zipper::Zipper::trace_uid(&*_rz_b)); AlgebraicStatus::Element }
    fn subtract_into<Z: ZipperInfallibleSubtries<V, A>>(&mut self, _read_zipper: &Z, _prune: bool) -> AlgebraicStatus where V: DistributiveLattice { crate::trace::trace_point!(subtract_into, self.uid, crate::zipper::Zipper::trace_uid(&*_read_zipper), _prune); AlgebraicStatus::Element }
    fn restrict<Z: ZipperInfallibleSubtries<V, A>>(&mut self, _read_zipper: &Z) -> AlgebraicStatus { crate::trace::trace_point!(restrict, self.uid, crate::zipper::Zipper::trace_uid(&*_read_zipper)); AlgebraicStatus::Element }
    fn restricting<Z: ZipperInfallibleSubtries<V, A>>(&mut self, _read_zipper: &Z) -> bool { crate::trace::trace_point!(restricting, self.uid, crate::zipper::Zipper::trace_uid(&*_read_zipper)); false }
    fn remove_branches(&mut self, prune: bool) -> bool { crate::trace::trace_point!(remove_branches, self.uid, prune); false }
    fn take_map(&mut self, prune: bool) -> Option<PathMap<V, A>> { crate::trace::trace_point!(take_map, self.uid, prune); None }
    fn remove_unmasked_branches(&mut self, _mask: ByteMask, prune: bool) { crate::trace::trace_point!(remove_unmasked_branches, self.uid, core::ptr::from_ref(&_mask), prune);}
    fn create_path(&mut self) -> bool { crate::trace::trace_point!(create_path, self.uid); false }
    fn prune_path(&mut self) -> usize { crate::trace::trace_point!(prune_path, self.uid); 0 }
    fn prune_ascend(&mut self) -> usize { crate::trace::trace_point!(prune_ascend, self.uid); 0 }
}
