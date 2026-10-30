
use crate::zipper::*;
use crate::utils::ByteMask;

/// A [Zipper] type that wraps two other zippers that are expected to behave identically,
/// and panics when they don't.  Useful to debug a Zipper implementation.
pub struct DiffZipper<A: Zipper, B: Zipper> {
    /// Identifies the zipper in traces, see [crate::trace]
    uid: crate::trace::ZipperUid,
    a: A,
    b: B,
    log_moves: bool,
}

impl<A: Zipper, B: Zipper> DiffZipper<A, B> {
    pub fn new(a: A, b: B, log_moves: bool) -> Self {
        Self{ uid: crate::trace::new_uid!(DiffZipper),a, b, log_moves}
    }
}

impl<A: Zipper, B: Zipper> Zipper for DiffZipper<A, B>
{
    fn trace_uid(&self) -> u64 { self.uid.get() }
    fn path_exists(&self) -> bool {
        crate::trace::trace_point!(path_exists, self.uid);
        let a = self.a.path_exists();
        let b = self.b.path_exists();
        assert_eq!(a, b);
        a
    }
    fn is_val(&self) -> bool {
        crate::trace::trace_point!(is_val, self.uid);
        let a = self.a.is_val();
        let b = self.b.is_val();
        assert_eq!(a, b);
        a
    }
    fn child_count(&self) -> usize {
        crate::trace::trace_point!(child_count, self.uid);
        let a = self.a.child_count();
        let b = self.b.child_count();
        assert_eq!(a, b);
        a
    }
    fn child_mask(&self) -> ByteMask {
        crate::trace::trace_point!(child_mask, self.uid);
        let a = self.a.child_mask();
        let b = self.b.child_mask();
        assert_eq!(a, b);
        a
    }
}

impl<A: Zipper + ZipperMoving, B: Zipper + ZipperMoving> ZipperMoving for DiffZipper<A, B>
{
    fn depth(&self) -> usize {
        crate::trace::trace_point!(depth, self.uid);
        let a = self.a.depth();
        let b = self.b.depth();
        assert_eq!(a, b);
        a
    }
    fn at_root(&self) -> bool {
        crate::trace::trace_point!(at_root, self.uid);
        let a = self.a.at_root();
        let b = self.b.at_root();
        assert_eq!(a, b);
        a
    }
    fn focus_byte(&self) -> Option<u8> {
        crate::trace::trace_point!(focus_byte, self.uid);
        let a = self.a.focus_byte();
        let b = self.b.focus_byte();
        assert_eq!(a, b);
        a
    }
    fn reset(&mut self) {
        crate::trace::trace_point!(reset, self.uid);
        self.a.reset();
        self.b.reset();
        if self.log_moves {
            println!("DiffZipper: reset")
        }
    }
    fn val_count(&self) -> usize {
        crate::trace::trace_point!(val_count, self.uid);
        let a = self.a.val_count();
        let b = self.b.val_count();
        assert_eq!(a, b);
        a
    }
    fn descend_to<P: AsRef<[u8]>>(&mut self, path: P) {
        crate::trace::trace_point!(descend_to, self.uid, AsRef::<[u8]>::as_ref(&path).as_ptr(), AsRef::<[u8]>::as_ref(&path).len());
        let path = path.as_ref();
        self.a.descend_to(path);
        self.b.descend_to(path);
        if self.log_moves {
            println!("DiffZipper: descend_to path={path:?}")
        }
        assert_eq!(self.a.path_exists(), self.b.path_exists());
    }
    fn descend_to_existing<P: AsRef<[u8]>>(&mut self, path: P) -> usize {
        crate::trace::trace_point!(descend_to_existing, self.uid, AsRef::<[u8]>::as_ref(&path).as_ptr(), AsRef::<[u8]>::as_ref(&path).len());
        let path = path.as_ref();
        let a = self.a.descend_to_existing(path);
        let b = self.b.descend_to_existing(path);
        if self.log_moves {
            println!("DiffZipper: descend_to_existing path={path:?}")
        }
        assert_eq!(a, b);
        a
    }
    fn descend_to_val<K: AsRef<[u8]>>(&mut self, path: K) -> usize {
        crate::trace::trace_point!(descend_to_val, self.uid, AsRef::<[u8]>::as_ref(&path).as_ptr(), AsRef::<[u8]>::as_ref(&path).len());
        let path = path.as_ref();
        let a = self.a.descend_to_val(path);
        let b = self.b.descend_to_val(path);
        if self.log_moves {
            println!("DiffZipper: descend_to_val path={path:?}")
        }
        assert_eq!(a, b);
        a
    }
    fn descend_to_byte(&mut self, k: u8) {
        crate::trace::trace_point!(descend_to_byte, self.uid, k);
        self.a.descend_to_byte(k);
        self.b.descend_to_byte(k);
        if self.log_moves {
            println!("DiffZipper: descend_to_byte k={k}")
        }
        assert_eq!(self.a.path_exists(), self.b.path_exists());
    }
    fn descend_indexed_byte(&mut self, idx: usize) -> Option<u8> {
        crate::trace::trace_point!(descend_indexed_byte, self.uid, idx);
        let a = self.a.descend_indexed_byte(idx);
        let b = self.b.descend_indexed_byte(idx);
        if self.log_moves {
            println!("DiffZipper: descend_indexed_byte idx={idx}")
        }
        assert_eq!(a, b);
        a
    }
    fn descend_first_byte(&mut self) -> Option<u8> {
        crate::trace::trace_point!(descend_first_byte, self.uid);
        let a = self.a.descend_first_byte();
        let b = self.b.descend_first_byte();
        if self.log_moves {
            println!("DiffZipper: descend_first_byte")
        }
        assert_eq!(a, b);
        a
    }
    fn descend_until_observed<Obs: PathObserver>(&mut self, obs: &mut Obs) -> bool {
        crate::trace::trace_point!(descend_until_observed, self.uid);
        //`a`'s descent is forwarded to the caller as it happens, since a digest can't reconstruct
        //the bytes afterwards.  `b`'s is only hashed, and the two digests are compared.
        let mut hash_a = HashObserver::default();
        let mut hash_b = HashObserver::default();
        let a = self.a.descend_until_observed(&mut (&mut hash_a, &mut *obs));
        let b = self.b.descend_until_observed(&mut hash_b);
        if self.log_moves {
            println!("DiffZipper: descend_until")
        }
        assert_eq!(a, b);
        assert_eq!(hash_a, hash_b);
        a
    }
    fn ascend(&mut self, steps: usize) -> usize {
        crate::trace::trace_point!(ascend, self.uid, steps);
        let a = self.a.ascend(steps);
        let b = self.b.ascend(steps);
        if self.log_moves {
            println!("DiffZipper: ascend steps={steps}")
        }
        assert_eq!(a, b);
        a
    }
    fn ascend_byte(&mut self) -> bool {
        crate::trace::trace_point!(ascend_byte, self.uid);
        let a = self.a.ascend_byte();
        let b = self.b.ascend_byte();
        if self.log_moves {
            println!("DiffZipper: ascend_byte")
        }
        assert_eq!(a, b);
        a
    }
    fn ascend_until(&mut self) -> usize {
        crate::trace::trace_point!(ascend_until, self.uid);
        let a = self.a.ascend_until();
        let b = self.b.ascend_until();
        if self.log_moves {
            println!("DiffZipper: ascend_until")
        }
        assert_eq!(a, b);
        a
    }
    fn ascend_until_branch(&mut self) -> usize {
        crate::trace::trace_point!(ascend_until_branch, self.uid);
        let a = self.a.ascend_until_branch();
        let b = self.b.ascend_until_branch();
        if self.log_moves {
            println!("DiffZipper: ascend_until_branch")
        }
        assert_eq!(a, b);
        a
    }
    fn to_next_sibling_byte(&mut self) -> Option<u8> {
        crate::trace::trace_point!(to_next_sibling_byte, self.uid);
        let a = self.a.to_next_sibling_byte();
        let b = self.b.to_next_sibling_byte();
        if self.log_moves {
            println!("DiffZipper: to_next_sibling_byte")
        }
        assert_eq!(a, b);
        a
    }
    fn to_prev_sibling_byte(&mut self) -> Option<u8> {
        crate::trace::trace_point!(to_prev_sibling_byte, self.uid);
        let a = self.a.to_prev_sibling_byte();
        let b = self.b.to_prev_sibling_byte();
        if self.log_moves {
            println!("DiffZipper: to_prev_sibling_byte")
        }
        assert_eq!(a, b);
        a
    }
}

impl<A: Zipper + ZipperPath, B: Zipper + ZipperPath> ZipperPath for DiffZipper<A, B>
{
    fn path(&self) -> &[u8] {
        crate::trace::trace_point!(path, self.uid);
        let a = self.a.path();
        let b = self.b.path();
        assert_eq!(a, b);
        a
    }
}

impl<A: Zipper + ZipperAbsolutePath, B: Zipper + ZipperAbsolutePath> ZipperAbsolutePath for DiffZipper<A, B>
{
    fn origin_path(&self) -> &[u8] {
        crate::trace::trace_point!(origin_path, self.uid);
        let a = self.a.origin_path();
        let b = self.b.origin_path();
        assert_eq!(a, b);
        a
    }
    fn root_prefix_path(&self) -> &[u8] {
        crate::trace::trace_point!(root_prefix_path, self.uid);
        let a = self.a.root_prefix_path();
        let b = self.b.root_prefix_path();
        assert_eq!(a, b);
        a
    }
}

impl<A: Zipper + ZipperPathBuffer, B: Zipper + ZipperPathBuffer> ZipperPathBuffer for DiffZipper<A, B>
{
    unsafe fn origin_path_assert_len(&self, len: usize) -> &[u8] {
        let a = unsafe{ self.a.origin_path_assert_len(len) };
        let b = unsafe{ self.b.origin_path_assert_len(len) };
        assert_eq!(a, b);
        a
    }
    fn reserve_buffers(&mut self, path_len: usize, stack_depth: usize) {
        self.a.reserve_buffers(path_len, stack_depth);
        self.b.reserve_buffers(path_len, stack_depth);
    }

    fn prepare_buffers(&mut self) {
        self.a.prepare_buffers();
        self.b.prepare_buffers();
    }
}

impl<A: Zipper + ZipperIteration, B: Zipper + ZipperIteration> ZipperIteration for DiffZipper<A, B>
{
    fn to_next_val_observed<Obs: PathObserver>(&mut self, obs: &mut Obs) -> bool {
        crate::trace::trace_point!(to_next_val_observed, self.uid);
        let a = self.a.to_next_val_observed(obs);
        let b = self.b.to_next_val();
        if self.log_moves {
            println!("DiffZipper: to_next_val")
        }
        assert_eq!(a, b);
        a
    }
    fn descend_last_path_observed<Obs: PathObserver>(&mut self, obs: &mut Obs) -> bool {
        crate::trace::trace_point!(descend_last_path_observed, self.uid);
        //`a`'s descent is forwarded to the caller as it happens, since a digest can't reconstruct
        //the bytes afterwards.  `b`'s is only hashed, and the two digests are compared.
        let mut hash_a = HashObserver::default();
        let mut hash_b = HashObserver::default();
        let a = self.a.descend_last_path_observed(&mut (&mut hash_a, &mut *obs));
        let b = self.b.descend_last_path_observed(&mut hash_b);
        if self.log_moves {
            println!("DiffZipper: descend_last_path")
        }
        assert_eq!(a, b);
        assert_eq!(hash_a, hash_b);
        a
    }
    fn descend_first_k_path_observed<Obs: PathObserver>(&mut self, k: usize, obs: &mut Obs) -> bool {
        crate::trace::trace_point!(descend_first_k_path_observed, self.uid, k);
        let a = self.a.descend_first_k_path_observed(k, obs);
        let b = self.b.descend_first_k_path(k);
        if self.log_moves {
            println!("DiffZipper: descend_first_k_path k={k}")
        }
        assert_eq!(a, b);
        a
    }
    fn to_next_k_path_observed<Obs: PathObserver>(&mut self, k: usize, obs: &mut Obs) -> bool {
        crate::trace::trace_point!(to_next_k_path_observed, self.uid, k);
        let a = self.a.to_next_k_path_observed(k, obs);
        let b = self.b.to_next_k_path(k);
        if self.log_moves {
            println!("DiffZipper: to_next_k_path k={k}")
        }
        assert_eq!(a, b);
        a
    }
}

impl <PZL : ZipperProduct, PZR : ZipperProduct> ZipperProduct for DiffZipper<PZL, PZR> {
    fn focus_factor(&self) -> usize {
        let a = self.a.focus_factor();
        let b = self.b.focus_factor();
        assert_eq!(a, b);
        a
    }
    fn factor_count(&self) -> usize {
        let a = self.a.factor_count();
        let b = self.b.factor_count();
        assert_eq!(a, b);
        a
    }
    fn path_indices(&self) -> &[usize] {
        let a = self.a.path_indices();
        let b = self.b.path_indices();
        assert_eq!(a, b);
        a
    }
}
