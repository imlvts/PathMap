use divan::{Divan, Bencher, black_box};
use core::convert::Infallible;
use pathmap::alloc::GlobalAlloc;
use pathmap::morphisms::CatamorphismCached;
use pathmap::utils::ByteMask;
use pathmap::utils::ints::gen_int_range;
use pathmap::PathMap;

fn main() {
    // Run registered benchmarks.
    let divan = Divan::from_args()
        .sample_count(4000);

    divan.main();
}

fn build_map(count: u64) -> PathMap<()> {
    // Dense range of u64 keys encoded as paths; sized to keep benches fast and stable.
    gen_int_range::<(), 8, u64>(0, count, 1, ())
}

const MAP_COUNT: u64 = 20_000_000;

// A complete binary trie keeps every internal node as a two-entry LineListNode.
const BINARY_TREE_DEPTH: usize = 18;
const BINARY_TREE_LEAF_COUNT: usize = 1 << BINARY_TREE_DEPTH;

fn build_binary_tree_map() -> PathMap<()> {
    let mut map = PathMap::new();
    for leaf in 0..BINARY_TREE_LEAF_COUNT {
        let mut path = [0u8; BINARY_TREE_DEPTH];
        for (level, byte) in path.iter_mut().enumerate() {
            *byte = ((leaf >> (BINARY_TREE_DEPTH - level - 1)) & 1) as u8;
        }
        map.insert(path, ());
    }
    map
}

#[divan::bench()]
fn factored_cata_jumping_val_count(bencher: Bencher) {
    let map = build_map(MAP_COUNT);
    let mut sink = 0usize;
    bencher.bench_local(|| {
        let rz = map.read_zipper();
        *black_box(&mut sink) = CatamorphismCached::<(), GlobalAlloc>::factored_cata_jumping::<_, _, Infallible, _, _, _, _, _, false>(&rz,
            |_| Ok(0usize),
            |_mask, w: usize, total| { *total += w; Ok(()) },
            |_val, _| Ok(1),
            |_mask, total, _| Ok(total.unwrap_or(0)),
            |_val, below| Ok(1 + below),
        ).unwrap();
    });
    assert_eq!(sink, MAP_COUNT as usize);
}

#[divan::bench()]
fn factored_cata_binary_tree_leaf_count(bencher: Bencher) {
    let map = build_binary_tree_map();
    let mut sink = 0usize;
    bencher.bench_local(|| {
        let rz = map.read_zipper();
        *black_box(&mut sink) = CatamorphismCached::<(), GlobalAlloc>
            ::factored_cata_jumping::<_, _, Infallible, _, _, _, _, _, false>(&rz,
                |_| Ok(0usize),
                |_mask, child_count: usize, total| {
                    *total += child_count;
                    Ok(())
                },
                // A leaf is a value with nothing below it
                |_value, _| Ok(1),
                |_mask, total, _| Ok(total.unwrap_or(0)),
                |_value, below| Ok(below),
            )
            .unwrap();
    });
    assert_eq!(sink, BINARY_TREE_LEAF_COUNT);
}

#[divan::bench()]
fn factored_cata_jumping_total_len(bencher: Bencher) {
    let map = build_map(MAP_COUNT);
    let mut sink = (0usize, 0usize);
    bencher.bench_local(|| {
        let rz = map.read_zipper();
        *black_box(&mut sink) = CatamorphismCached::<(), GlobalAlloc>::factored_cata_jumping::<_, _, Infallible, _, _, _, _, _, true>(&rz,
            |_| Ok((0usize, 0usize)),
            |_mask: &ByteMask, w: (usize, usize), acc: &mut (usize, usize)| {
                acc.0 += w.0;
                // Every folded child hangs below exactly one branch byte. `prefix` accounts
                // for compressed runs separately in `summarize_f` below.
                acc.1 += w.1 + w.0;
                Ok(())
            },
            // A leaf's own path runs through `prefix`
            |_val: &(), prefix| Ok((1usize, prefix.len())),
            |_mask: &ByteMask, acc, prefix| {
                let (count, total_len) = acc.unwrap_or((0, 0));
                Ok((count, total_len + count * prefix.len()))
            },
            |_val, (count, total_len): (usize, usize)| Ok((count + 1, total_len)),
        ).unwrap();
    });
    assert_eq!(sink, (MAP_COUNT as usize, MAP_COUNT as usize * 8));
}
