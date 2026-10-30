
use crate::zipper::*;
use crate::utils::ByteMask;

/// A [`Zipper`] type that moves over a completely empty trie
#[derive(Clone)]
pub struct EmptyZipper {
    /// Identifies the zipper in traces, see [crate::trace]
    uid: crate::trace::ZipperUid,
    path_start_idx: usize,
    path: Vec<u8> //NOTE: we could init this lazily and take a borrowed path, but I can't see a use case where that would matter
}

impl Default for EmptyZipper {
    fn default() -> Self {
        Self {
            uid: crate::trace::new_uid!(EmptyZipper),
            path_start_idx: 0,
            path: Vec::new(),
        }
    }
}

impl EmptyZipper {
    /// Returns a new `EmptyZipper` starting at the root
    pub fn new() -> Self {
        Self::default()
    }
    /// Returns a new `EmptyZipper` with the provided [`root_prefix_path`](ZipperAbsolutePath::root_prefix_path)
    pub fn new_at_path<K: AsRef<[u8]>>(path: K) -> Self {
        let path = path.as_ref();
        Self {
            uid: crate::trace::new_uid!(EmptyZipper),
            path_start_idx: path.len(),
            path: path.to_vec(),
        }
    }
}

impl Zipper for EmptyZipper {
    fn trace_uid(&self) -> u64 { self.uid.get() }
    fn path_exists(&self) -> bool { crate::trace::trace_point!(path_exists, self.uid); false }
    fn is_val(&self) -> bool { crate::trace::trace_point!(is_val, self.uid); false }
    fn child_count(&self) -> usize { crate::trace::trace_point!(child_count, self.uid); 0 }
    fn child_mask(&self) -> ByteMask { crate::trace::trace_point!(child_mask, self.uid); ByteMask::EMPTY }
}

impl ZipperMoving for EmptyZipper {
    #[inline] fn depth(&self) -> usize { crate::trace::trace_point!(depth, self.uid); self.path.len() - self.path_start_idx }
    #[inline]
    fn focus_byte(&self) -> Option<u8> { crate::trace::trace_point!(focus_byte, self.uid); self.path.last().cloned() }
    fn reset(&mut self) { crate::trace::trace_point!(reset, self.uid); self.path.truncate(self.path_start_idx) }
    fn val_count(&self) -> usize { crate::trace::trace_point!(val_count, self.uid); 0 }
    fn descend_to<K: AsRef<[u8]>>(&mut self, k: K) {
        crate::trace::trace_point!(descend_to, self.uid, AsRef::<[u8]>::as_ref(&k).as_ptr(), AsRef::<[u8]>::as_ref(&k).len());
        self.path.extend_from_slice(k.as_ref());
    }
    fn descend_to_byte(&mut self, k: u8) {
        crate::trace::trace_point!(descend_to_byte, self.uid, k);
        self.path.push(k);
    }
    fn descend_indexed_byte(&mut self, _idx: usize) -> Option<u8> { crate::trace::trace_point!(descend_indexed_byte, self.uid, _idx); None }
    fn descend_first_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(descend_first_byte, self.uid); None }
    fn descend_until_observed<Obs: PathObserver>(&mut self, _obs: &mut Obs) -> bool { crate::trace::trace_point!(descend_until_observed, self.uid); false }
    fn ascend(&mut self, steps: usize) -> usize {
        crate::trace::trace_point!(ascend, self.uid, steps);
        let available = self.path.len() - self.path_start_idx;
        let ascended = steps.min(available);
        self.path.truncate(self.path.len() - ascended);
        ascended
    }
    fn ascend_byte(&mut self) -> bool {
        crate::trace::trace_point!(ascend_byte, self.uid);
        if self.path.len() > self.path_start_idx {
            self.path.pop();
            true
        } else {
            false
        }
    }
    fn ascend_until(&mut self) -> usize {
        crate::trace::trace_point!(ascend_until, self.uid);
        let ascended = self.path.len() - self.path_start_idx;
        self.reset();
        ascended
    }
    fn ascend_until_branch(&mut self) -> usize {
        crate::trace::trace_point!(ascend_until_branch, self.uid);
        self.ascend_until()
    }
    fn to_next_sibling_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(to_next_sibling_byte, self.uid); None }
    fn to_prev_sibling_byte(&mut self) -> Option<u8> { crate::trace::trace_point!(to_prev_sibling_byte, self.uid); None }
}

impl ZipperPath for EmptyZipper {
    fn path(&self) -> &[u8] { crate::trace::trace_point!(path, self.uid); &self.path[self.path_start_idx..] }
}

impl ZipperAbsolutePath for EmptyZipper {
    fn origin_path(&self) -> &[u8] { crate::trace::trace_point!(origin_path, self.uid); &self.path }
    fn root_prefix_path(&self) -> &[u8] { crate::trace::trace_point!(root_prefix_path, self.uid); &self.path[..self.path_start_idx] }
}

impl ZipperIteration for EmptyZipper {
    fn to_next_val_observed<Obs: PathObserver>(&mut self, _obs: &mut Obs) -> bool { crate::trace::trace_point!(to_next_val_observed, self.uid); false }
    fn descend_last_path_observed<Obs: PathObserver>(&mut self, _obs: &mut Obs) -> bool { crate::trace::trace_point!(descend_last_path_observed, self.uid); false }
    fn descend_first_k_path_observed<Obs: PathObserver>(&mut self, _k: usize, _obs: &mut Obs) -> bool { crate::trace::trace_point!(descend_first_k_path_observed, self.uid, _k); false }
    fn to_next_k_path_observed<Obs: PathObserver>(&mut self, k: usize, obs: &mut Obs) -> bool {
        crate::trace::trace_point!(to_next_k_path_observed, self.uid, k);
        let ascended = self.ascend(k);
        obs.ascend(ascended);
        false
    }
}

impl<V> ZipperValues<V> for EmptyZipper {
    fn val(&self) -> Option<&V> { crate::trace::trace_point!(val, self.uid); None }
}

impl<V> ZipperValuesAt<V> for EmptyZipper {
    fn val_at<K: AsRef<[u8]>>(&self, _path: K) -> Option<&V> { crate::trace::trace_point!(val_at, self.uid, AsRef::<[u8]>::as_ref(&_path).as_ptr(), AsRef::<[u8]>::as_ref(&_path).len()); None }
}

impl<V> ZipperForking<V> for EmptyZipper {
    type ReadZipperT<'a> = EmptyZipper;
    fn fork_read_zipper<'a>(&'a self) -> Option<Self::ReadZipperT<'a>> { crate::trace::trace_point!(fork_read_zipper, self.uid); None }
}

impl<'a, V: Clone + Send + Sync> ZipperReadOnlyValues<'a, V> for EmptyZipper {
    fn get_val(&self) -> Option<&'a V> { crate::trace::trace_point!(get_val, self.uid); None }
    fn get_val_at<K: AsRef<[u8]>>(&self, _path: K) -> Option<&'a V> { crate::trace::trace_point!(get_val_at, self.uid, AsRef::<[u8]>::as_ref(&_path).as_ptr(), AsRef::<[u8]>::as_ref(&_path).len()); None }
}

impl<'a, V: Clone + Send + Sync> ZipperReadOnlyConditionalValues<'a, V> for EmptyZipper {
    type WitnessT = ();
    fn witness<'w>(&self) -> Self::WitnessT { () }
    fn get_val_with_witness<'w>(&self, _witness: &'w Self::WitnessT) -> Option<&'w V> where 'a: 'w { crate::trace::trace_point!(get_val_with_witness, self.uid); None }
}

impl<'a, V: Clone + Send + Sync> ZipperReadOnlyIteration<'a, V> for EmptyZipper {
    fn to_next_get_val_observed<Obs: PathObserver>(&mut self, _obs: &mut Obs) -> Option<&'a V> { crate::trace::trace_point!(to_next_get_val_observed, self.uid); None }
}

impl<'a, V: Clone + Send + Sync> ZipperReadOnlyConditionalIteration<'a, V> for EmptyZipper {
    fn to_next_get_val_with_witness_observed<'w, Obs: PathObserver>(&mut self, _witness: &'w Self::WitnessT, _obs: &mut Obs) -> Option<&'w V> where 'a: 'w { crate::trace::trace_point!(to_next_get_val_with_witness_observed, self.uid); None }
}

impl ZipperPathBuffer for EmptyZipper {
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

crate::impl_name_only_debug!(
    impl core::fmt::Debug for EmptyZipper
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_next_k_path_rewinds_and_reports_movement() {
        let mut z = EmptyZipper::new_at_path(b"root.");
        z.descend_to(b"abc");
        let mut observed = z.path().to_vec();

        assert!(!z.to_next_k_path_observed(2, &mut observed));
        assert_eq!(z.path(), b"a");
        assert_eq!(observed, z.path());
        assert_eq!(z.root_prefix_path(), b"root.");

        assert!(!z.to_next_k_path_observed(5, &mut observed));
        assert_eq!(z.path(), b"");
        assert_eq!(observed, z.path());
    }
}
