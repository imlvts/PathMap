//! Debug utilities for catamorphisms and other morphisms

use crate::utils::ByteMask;
use crate::alloc::Allocator;
use crate::PathMap;
use crate::zipper::*;
use crate::morphisms::factored_cata_jumping_debug_body;

/// Debug extension trait for catamorphisms
///
/// This trait provides debug-only catamorphism methods that may expose additional
/// information useful for debugging and development.
pub trait CatamorphismDebug<V> {
    /// A debug-only version of [`factored_cata_jumping`](crate::morphisms::CatamorphismCached::factored_cata_jumping)
    /// where the full absolute path is available to `map_f`, `summarize_f` and `collapse_f` as a
    /// trailing argument.
    ///
    /// Using data from the full path for your algorithm **will** lead to incorrect behavior.
    /// You must either adapt your algorithm not to require full path data or use one of the
    /// methods in [`crate::morphisms::CatamorphismSideEffecting`].
    ///
    fn factored_cata_jumping_debug<Acc, W, E, NewAccF, FoldChildF, MapF, SummarizeF, CollapseF>(
        &self,
        new_acc_f: NewAccF,
        fold_child_f: FoldChildF,
        map_f: MapF,
        summarize_f: SummarizeF,
        collapse_f: CollapseF,
    ) -> Result<W, E>
    where
        W: Clone,
        NewAccF: Copy + Fn(&ByteMask) -> Result<Acc, E>,
        FoldChildF: Copy + Fn(&ByteMask, W, &mut Acc) -> Result<(), E>,
        MapF: Copy + Fn(&V, &[u8], &[u8]) -> Result<W, E>,
        SummarizeF: Copy + Fn(&ByteMask, Option<Acc>, &[u8], &[u8]) -> Result<W, E>,
        CollapseF: Copy + Fn(&V, W, &[u8]) -> Result<W, E>;
}

impl<'a, Z, V: 'a> CatamorphismDebug<V> for Z where Z: Clone + Zipper + ZipperReadOnlyConditionalValues<'a, V> + ZipperConcrete + ZipperAbsolutePath + ZipperPathBuffer {
    fn factored_cata_jumping_debug<Acc, W, E, NewAccF, FoldChildF, MapF, SummarizeF, CollapseF>(&self, new_acc_f: NewAccF, fold_child_f: FoldChildF, map_f: MapF, summarize_f: SummarizeF, collapse_f: CollapseF) -> Result<W, E>
    where
        W: Clone,
        NewAccF: Copy + Fn(&ByteMask) -> Result<Acc, E>,
        FoldChildF: Copy + Fn(&ByteMask, W, &mut Acc) -> Result<(), E>,
        MapF: Copy + Fn(&V, &[u8], &[u8]) -> Result<W, E>,
        SummarizeF: Copy + Fn(&ByteMask, Option<Acc>, &[u8], &[u8]) -> Result<W, E>,
        CollapseF: Copy + Fn(&V, W, &[u8]) -> Result<W, E>,
    {
        factored_cata_jumping_debug_body(self.clone(), new_acc_f, fold_child_f, map_f, summarize_f, collapse_f)
    }
}

impl<V: 'static + Clone + Send + Sync + Unpin, A: Allocator + 'static> CatamorphismDebug<V> for PathMap<V, A> {
    fn factored_cata_jumping_debug<Acc, W, E, NewAccF, FoldChildF, MapF, SummarizeF, CollapseF>(&self, new_acc_f: NewAccF, fold_child_f: FoldChildF, map_f: MapF, summarize_f: SummarizeF, collapse_f: CollapseF) -> Result<W, E>
    where
        W: Clone,
        NewAccF: Copy + Fn(&ByteMask) -> Result<Acc, E>,
        FoldChildF: Copy + Fn(&ByteMask, W, &mut Acc) -> Result<(), E>,
        MapF: Copy + Fn(&V, &[u8], &[u8]) -> Result<W, E>,
        SummarizeF: Copy + Fn(&ByteMask, Option<Acc>, &[u8], &[u8]) -> Result<W, E>,
        CollapseF: Copy + Fn(&V, W, &[u8]) -> Result<W, E>,
    {
        self.read_zipper().factored_cata_jumping_debug(new_acc_f, fold_child_f, map_f, summarize_f, collapse_f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::morphisms::CatamorphismCached;

    #[test]
    fn debug_cached_cata_uses_the_zipper_focus_and_absolute_paths() {
        let map: PathMap<()> = [
            (b"a".as_slice(), ()),
            (b"ab".as_slice(), ()),
            (b"ac".as_slice(), ()),
            (b"z".as_slice(), ()),
        ]
        .into_iter()
        .collect();
        let mut zipper = map.read_zipper();
        zipper.descend_to(b"a");
        let paths = std::cell::RefCell::new(Vec::new());

        let count = zipper.factored_cata_jumping_debug::<usize, usize, core::convert::Infallible, _, _, _, _, _>(
            |_| Ok(0),
            |_mask, child, total| { *total += child; Ok(()) },
            |_value, _prefix, path| {
                paths.borrow_mut().push(path.to_vec());
                Ok(1)
            },
            |_mask, total, _prefix, path| {
                paths.borrow_mut().push(path.to_vec());
                Ok(total.unwrap_or(0))
            },
            |_value, below, path| {
                paths.borrow_mut().push(path.to_vec());
                Ok(1 + below)
            },
        ).unwrap();

        assert_eq!(count, 3);
        assert!(paths.borrow().iter().all(|path| path.starts_with(b"a")));
        assert!(paths.borrow().contains(&b"a".to_vec()));
        assert_eq!(zipper.path(), b"a");
    }

    #[test]
    fn debug_cached_cata_does_not_ascend_past_a_unary_focus() {
        let map: PathMap<()> = [(b"abc".as_slice(), ()), (b"z".as_slice(), ())]
            .into_iter()
            .collect();
        let mut zipper = map.read_zipper();
        zipper.descend_to(b"a");
        let paths = std::cell::RefCell::new(Vec::new());

        let count = zipper.factored_cata_jumping_debug::<usize, usize, core::convert::Infallible, _, _, _, _, _>(
            |_| Ok(0),
            |_mask, child, total| { *total += child; Ok(()) },
            |_value, _prefix, path| {
                paths.borrow_mut().push(path.to_vec());
                Ok(1)
            },
            |_mask, total, _prefix, path| {
                paths.borrow_mut().push(path.to_vec());
                Ok(total.unwrap_or(0))
            },
            |_value, below, path| {
                paths.borrow_mut().push(path.to_vec());
                Ok(1 + below)
            },
        ).unwrap();

        assert_eq!(count, 1);
        assert!(paths.borrow().iter().all(|path| path.starts_with(b"a")));
        assert_eq!(zipper.path(), b"a");
    }

    #[test]
    fn debug_cached_cata_short_circuits_shared_subtries() {
        let child: PathMap<()> = [(b"c".as_slice(), ()), (b"d".as_slice(), ())]
            .into_iter()
            .collect();
        let mut map = PathMap::new();
        let mut writer = map.write_zipper();
        for path in [b"a".as_slice(), b"b".as_slice()] {
            writer.reset();
            writer.descend_to(path);
            writer.graft_map(child.clone());
        }
        drop(writer);

        let cached_calls = std::sync::atomic::AtomicUsize::new(0);
        let cached = CatamorphismCached::factored_cata_jumping::<usize, usize, core::convert::Infallible, _, _, _, _, _, true>(&map,
            |_| Ok(0),
            |_mask, child, total| { *total += child; Ok(()) },
            |_value, _prefix| {
                cached_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(1)
            },
            |_mask, total, _prefix| {
                cached_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(total.unwrap_or(0))
            },
            |_value, below| Ok(1 + below),
        ).unwrap();

        let debug_calls = std::sync::atomic::AtomicUsize::new(0);
        let debug = map.factored_cata_jumping_debug::<usize, usize, core::convert::Infallible, _, _, _, _, _>(
            |_| Ok(0),
            |_mask, child, total| { *total += child; Ok(()) },
            |_value, _prefix, _path| {
                debug_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(1)
            },
            |_mask, total, _prefix, _path| {
                debug_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(total.unwrap_or(0))
            },
            |_value, below, _path| Ok(1 + below),
        ).unwrap();

        assert_eq!(cached, 4);
        assert_eq!(debug, cached);
        assert_eq!(debug_calls.load(std::sync::atomic::Ordering::Relaxed), cached_calls.load(std::sync::atomic::Ordering::Relaxed));
    }
}
