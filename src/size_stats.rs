//! Memory footprint and byte-occupancy measurement for a [PathMap]'s node graph.
//!
//! [SizeStats::measure] walks every reachable node once (nodes shared between several parents
//! are counted a single time) and records, per node type, how many nodes exist, how many bytes
//! they request from the allocator, and how much of that space carries information (key bytes,
//! onward links, values) versus padding, unused slots, or spare `Vec` capacity.
//!
//! Two views of size are reported:
//! - **requested bytes**: the sum of `size_of` for every heap block a node owns (its own box,
//!   the entries vector of a byte node, the boxed cofrees of a cell node, heap-spilled values).
//! - **estimated malloc bytes**: the same blocks after rounding the way glibc's `malloc` does
//!   (8-byte header, 16-byte granularity, 32-byte minimum chunk). Small nodes pay a lot here.
//!
//! The `size_stats` example drives this over a set of datasets and cross-checks the structural
//! estimate against a counting global allocator.

use std::collections::{HashMap, HashSet};
use core::mem::size_of;

use crate::PathMap;
use crate::alloc::Allocator;
use crate::trie_node::{TaggedNodeRef, TrieNode, TrieNodeODRc};
use crate::line_list_node::{LineListNode, KEY_BYTES_CNT};
use crate::dense_byte_node::{CoFreeSlotStats, DenseByteNode, CellByteNode, OrdinaryCoFree, CellCoFree};

/// Rounds a requested allocation size up to the chunk glibc `malloc` would hand out
pub fn glibc_chunk_size(requested: usize) -> usize {
    if requested == 0 { return 0; }
    let with_header = requested + 8;
    let rounded = (with_header + 15) & !15;
    rounded.max(32)
}

/// Rounds a requested allocation size up to the jemalloc size class that would serve it
/// (8-byte classes up to 16, 16-byte classes up to 128, then four classes per doubling).
/// jemalloc keeps no per-allocation header, so this is the whole cost.
pub fn jemalloc_size_class(requested: usize) -> usize {
    match requested {
        0 => 0,
        1..=16 => (requested + 7) & !7,
        17..=128 => (requested + 15) & !15,
        _ => {
            let group = (usize::BITS - (requested - 1).leading_zeros()) as usize; // requested in (2^(g-1), 2^g]
            let step = 1usize << (group - 3); // four classes per doubling
            (requested + step - 1) & !(step - 1)
        }
    }
}

/// Number of buckets in [SizeStats::dense_items_hist]. Bucket `i` holds nodes with
/// `2^(i-1) < items <= 2^i` occupied entries; bucket 0 holds nodes with exactly one entry.
pub const DENSE_HIST_BUCKETS: usize = 9;

/// Maps an entry count (1..=256) onto a [DENSE_HIST_BUCKETS] bucket index
pub fn dense_bucket(items: usize) -> usize {
    if items <= 1 { 0 } else { (usize::BITS - (items - 1).leading_zeros()) as usize }
}

/// Label for a [dense_bucket] index
pub fn dense_bucket_label(bucket: usize) -> String {
    match bucket {
        0 => "1".to_string(),
        1 => "2".to_string(),
        b => format!("{}-{}", (1usize << (b - 1)) + 1, 1usize << b),
    }
}

/// Per-node-type totals
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KindStats {
    /// Number of nodes of this kind
    pub nodes: usize,
    /// Number of heap blocks owned by nodes of this kind (the node box, plus any side allocations)
    pub allocations: usize,
    /// Bytes requested from the allocator for all blocks owned by nodes of this kind
    pub requested_bytes: usize,
    /// [requested_bytes](Self::requested_bytes) after glibc chunk rounding
    pub malloc_bytes: usize,
    /// [requested_bytes](Self::requested_bytes) after jemalloc size-class rounding
    pub jemalloc_bytes: usize,
    /// Key bytes materialised inside nodes of this kind (one per dense entry, `key_len` per list slot)
    pub key_bytes: usize,
    /// Onward links (child pointers) held by nodes of this kind
    pub child_links: usize,
    /// Values held by nodes of this kind
    pub values: usize,
}

impl KindStats {
    fn add_block(&mut self, requested: usize) {
        if requested > 0 {
            self.allocations += 1;
            self.requested_bytes += requested;
            self.malloc_bytes += glibc_chunk_size(requested);
            self.jemalloc_bytes += jemalloc_size_class(requested);
        }
    }
}

/// The slot configuration of a [LineListNode]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ListShape {
    /// Nothing in either slot
    Empty,
    /// Slot 0 holds a value, slot 1 is free
    Val,
    /// Slot 0 holds a child, slot 1 is free
    Child,
    /// Both slots hold values
    ValVal,
    /// One slot holds a value, the other a child
    ValChild,
    /// Both slots hold children
    ChildChild,
}

impl ListShape {
    /// All shapes in display order
    pub const ALL: [ListShape; 6] = [Self::Empty, Self::Val, Self::Child, Self::ValVal, Self::ValChild, Self::ChildChild];
    fn from_node<V: Clone + Send + Sync, A: Allocator>(node: &LineListNode<V, A>) -> Self {
        let slot0 = node.is_used::<0>().then(|| node.is_child_ptr::<0>());
        let slot1 = node.is_used::<1>().then(|| node.is_child_ptr::<1>());
        match (slot0, slot1) {
            (None, _) => Self::Empty,
            (Some(false), None) => Self::Val,
            (Some(true), None) => Self::Child,
            (Some(false), Some(false)) => Self::ValVal,
            (Some(true), Some(true)) => Self::ChildChild,
            (Some(_), Some(_)) => Self::ValChild,
        }
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap()
    }
}

/// `size_of` for the types that make up a trie's nodes
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// `size_of::<V>()`
    pub value: usize,
    /// `size_of::<TrieNodeODRc<V, A>>()`
    pub node_ptr: usize,
    /// `size_of::<LineListNode<V, A>>()`
    pub list_node: usize,
    /// Key bytes a list node can hold
    pub list_key_bytes: usize,
    /// `size_of::<DenseByteNode<V, A>>()` (the header; entries live in a separate `Vec` block)
    pub dense_node: usize,
    /// `size_of::<OrdinaryCoFree<V, A>>()`, the per-entry cost of a dense node
    pub dense_entry: usize,
    /// `size_of::<CellByteNode<V, A>>()`
    pub cell_node: usize,
    /// `size_of::<CellCoFree<V, A>>()`, the per-entry cost of a cell node (a boxed [OrdinaryCoFree])
    pub cell_entry: usize,
}

impl Layout {
    /// Sizes for a trie holding `V`
    pub fn of<V: Clone + Send + Sync, A: Allocator>() -> Self {
        Self {
            value: size_of::<V>(),
            node_ptr: size_of::<TrieNodeODRc<V, A>>(),
            list_node: size_of::<LineListNode<V, A>>(),
            list_key_bytes: KEY_BYTES_CNT,
            dense_node: size_of::<DenseByteNode<V, A>>(),
            dense_entry: size_of::<OrdinaryCoFree<V, A>>(),
            cell_node: size_of::<CellByteNode<V, A>>(),
            cell_entry: size_of::<CellCoFree<V, A>>(),
        }
    }
}

/// Footprint and occupancy figures for one trie
#[derive(Clone, Debug)]
pub struct SizeStats {
    /// Totals for [LineListNode]s
    pub list: KindStats,
    /// Totals for [DenseByteNode]s
    pub dense: KindStats,
    /// Totals for [CellByteNode]s
    pub cell: KindStats,
    /// Nodes reached through more than one parent; counted once in the totals above
    pub shared_nodes: usize,
    /// Extra references those shared nodes receive beyond the first
    pub shared_extra_refs: usize,
    /// Number of values held by the counted nodes, including a root value. Nodes reached through
    /// several parents are counted once, so with structural sharing this is the number of value
    /// slots in memory rather than [PathMap::val_count]
    pub value_count: usize,
    /// `size_of::<V>()`
    pub value_size: usize,

    /// List nodes by [ListShape], indexed by [ListShape::index]
    pub list_shape_hist: [usize; 6],
    /// Key lengths of occupied list slots; index is the key length
    pub list_key_len_hist: [usize; KEY_BYTES_CNT + 1],
    /// List nodes whose only payload is a single child (pure path-compression links)
    pub list_chain_nodes: usize,
    /// List nodes holding exactly one value and nothing else (leaves)
    pub list_leaf_nodes: usize,
    /// Key lengths of the [list_leaf_nodes](Self::list_leaf_nodes); index is the key length
    pub list_leaf_key_len_hist: [usize; KEY_BYTES_CNT + 1],
    /// Key lengths of the [list_chain_nodes](Self::list_chain_nodes); index is the key length
    pub list_chain_key_len_hist: [usize; KEY_BYTES_CNT + 1],
    /// Byte nodes by exact occupied entry count (index = entries, 0..=256)
    pub dense_items_exact: Vec<usize>,
    /// Byte nodes whose entries are all value-only (no onward links); for a zero-sized `V` such a
    /// node is fully described by its mask
    pub dense_val_only_nodes: usize,
    /// Summed entries of [dense_val_only_nodes](Self::dense_val_only_nodes)
    pub dense_val_only_entries: usize,
    /// Summed capacity of [dense_val_only_nodes](Self::dense_val_only_nodes)
    pub dense_val_only_capacity: usize,
    /// Byte nodes by exact (entries, `Vec` capacity) pair
    pub dense_len_cap_hist: HashMap<(usize, usize), usize>,
    /// `size_of` the node structs and pointer types in this instantiation
    pub layout: Layout,

    /// Dense and cell nodes by occupied entry count, see [dense_bucket]
    pub dense_items_hist: [usize; DENSE_HIST_BUCKETS],
    /// Summed [CoFreeSlotStats] over dense and cell nodes
    pub dense_slots: CoFreeSlotStats,
    /// Dense nodes whose `Vec` capacity exceeds its length
    pub dense_overallocated_nodes: usize,
}

impl Default for SizeStats {
    fn default() -> Self {
        Self {
            list: KindStats::default(),
            dense: KindStats::default(),
            cell: KindStats::default(),
            shared_nodes: 0,
            shared_extra_refs: 0,
            value_count: 0,
            value_size: 0,
            list_shape_hist: [0; 6],
            list_key_len_hist: [0; KEY_BYTES_CNT + 1],
            list_chain_nodes: 0,
            list_leaf_nodes: 0,
            list_leaf_key_len_hist: [0; KEY_BYTES_CNT + 1],
            list_chain_key_len_hist: [0; KEY_BYTES_CNT + 1],
            dense_items_exact: vec![0; 257],
            dense_val_only_nodes: 0,
            dense_val_only_entries: 0,
            dense_val_only_capacity: 0,
            dense_len_cap_hist: HashMap::new(),
            layout: Layout::default(),
            dense_items_hist: [0; DENSE_HIST_BUCKETS],
            dense_slots: CoFreeSlotStats::default(),
            dense_overallocated_nodes: 0,
        }
    }
}

impl SizeStats {
    /// Walks `map` and gathers statistics
    pub fn measure<V: Clone + Send + Sync + Unpin, A: Allocator>(map: &PathMap<V, A>) -> Self {
        let mut stats = Self::default();
        stats.value_size = size_of::<V>();
        stats.layout = Layout::of::<V, A>();
        if map.root_val().is_some() {
            stats.value_count += 1;
        }
        if let Some(root) = map.root() {
            stats.walk(root);
        }
        stats
    }

    fn walk<V: Clone + Send + Sync, A: Allocator>(&mut self, root: &TrieNodeODRc<V, A>) {
        let mut seen: HashSet<u64> = HashSet::new();
        let mut stack: Vec<&TrieNodeODRc<V, A>> = vec![root];
        while let Some(node) = stack.pop() {
            if node.is_empty() {
                continue;
            }
            if node.refcount() > 1 {
                if !seen.insert(node.shared_node_id()) {
                    self.shared_extra_refs += 1;
                    continue;
                }
                self.shared_nodes += 1;
            }
            let tagged = node.as_tagged();
            self.count_node(&tagged);
            push_children(&tagged, &mut stack);
        }
    }

    fn count_node<V: Clone + Send + Sync, A: Allocator>(&mut self, node: &TaggedNodeRef<V, A>) {
        match node {
            TaggedNodeRef::LineListNode(list) => self.count_list(list),
            TaggedNodeRef::DenseByteNode(dense) => self.count_dense(dense),
            TaggedNodeRef::CellByteNode(cell) => self.count_cell(cell),
            #[cfg(feature = "bridge_nodes")]
            TaggedNodeRef::BridgeNode(_) => {},
            TaggedNodeRef::TinyRefNode(_) | TaggedNodeRef::EmptyNode => {},
        }
    }

    fn count_list<V: Clone + Send + Sync, A: Allocator>(&mut self, node: &LineListNode<V, A>) {
        let k = &mut self.list;
        k.nodes += 1;
        k.add_block(size_of::<LineListNode<V, A>>());
        let values_before = k.values;
        let shape = ListShape::from_node(node);
        self.list_shape_hist[shape.index()] += 1;
        let (key0, key1) = node.get_both_keys();
        let record_slot = |used: bool, is_child: bool, key: &[u8], k: &mut KindStats, hist: &mut [usize; KEY_BYTES_CNT + 1]| {
            if !used { return; }
            hist[key.len().min(KEY_BYTES_CNT)] += 1;
            k.key_bytes += key.len();
            if is_child {
                k.child_links += 1;
            } else {
                k.values += 1;
                if size_of::<V>() > 8 {
                    // LocalOrHeap spills values larger than its 8-byte inline buffer to the heap
                    k.add_block(size_of::<V>());
                }
            }
        };
        record_slot(node.is_used::<0>(), node.is_child_ptr::<0>(), key0, k, &mut self.list_key_len_hist);
        record_slot(node.is_used::<1>(), node.is_child_ptr::<1>(), key1, k, &mut self.list_key_len_hist);
        match shape {
            ListShape::Child => { self.list_chain_nodes += 1; self.list_chain_key_len_hist[key0.len().min(KEY_BYTES_CNT)] += 1; },
            ListShape::Val => { self.list_leaf_nodes += 1; self.list_leaf_key_len_hist[key0.len().min(KEY_BYTES_CNT)] += 1; },
            _ => {}
        }
        self.value_count += self.list.values - values_before;
    }

    fn count_dense<V: Clone + Send + Sync, A: Allocator>(&mut self, node: &DenseByteNode<V, A>) {
        let slots = node.cofree_slot_stats();
        let k = &mut self.dense;
        k.nodes += 1;
        k.add_block(size_of::<DenseByteNode<V, A>>());
        k.add_block(slots.capacity * size_of::<OrdinaryCoFree<V, A>>());
        Self::fold_slots(k, &mut self.dense_slots, &mut self.dense_items_hist, &mut self.dense_overallocated_nodes, &slots);
        self.dense_items_exact[slots.len.min(256)] += 1;
        *self.dense_len_cap_hist.entry((slots.len, slots.capacity)).or_insert(0) += 1;
        if slots.len > 0 && slots.val_only == slots.len {
            self.dense_val_only_nodes += 1;
            self.dense_val_only_entries += slots.len;
            self.dense_val_only_capacity += slots.capacity;
        }
        self.value_count += slots.val_only + slots.both;
    }

    fn count_cell<V: Clone + Send + Sync, A: Allocator>(&mut self, node: &CellByteNode<V, A>) {
        let slots = node.cofree_slot_stats();
        let k = &mut self.cell;
        k.nodes += 1;
        k.add_block(size_of::<CellByteNode<V, A>>());
        k.add_block(slots.capacity * size_of::<CellCoFree<V, A>>());
        for _ in 0..slots.len {
            k.add_block(size_of::<OrdinaryCoFree<V, A>>());
        }
        Self::fold_slots(k, &mut self.dense_slots, &mut self.dense_items_hist, &mut self.dense_overallocated_nodes, &slots);
        self.dense_items_exact[slots.len.min(256)] += 1;
        *self.dense_len_cap_hist.entry((slots.len, slots.capacity)).or_insert(0) += 1;
        if slots.len > 0 && slots.val_only == slots.len {
            self.dense_val_only_nodes += 1;
            self.dense_val_only_entries += slots.len;
            self.dense_val_only_capacity += slots.capacity;
        }
        self.value_count += slots.val_only + slots.both;
    }

    fn fold_slots(k: &mut KindStats, total: &mut CoFreeSlotStats, hist: &mut [usize; DENSE_HIST_BUCKETS], overalloc: &mut usize, slots: &CoFreeSlotStats) {
        k.key_bytes += slots.len;
        k.child_links += slots.rec_only + slots.both;
        k.values += slots.val_only + slots.both;
        total.len += slots.len;
        total.capacity += slots.capacity;
        total.rec_only += slots.rec_only;
        total.val_only += slots.val_only;
        total.both += slots.both;
        total.neither += slots.neither;
        if slots.len > 0 {
            hist[dense_bucket(slots.len)] += 1;
        }
        if slots.capacity > slots.len {
            *overalloc += 1;
        }
    }

    /// Total nodes of every kind
    pub fn total_nodes(&self) -> usize {
        self.list.nodes + self.dense.nodes + self.cell.nodes
    }
    /// Total heap blocks
    pub fn total_allocations(&self) -> usize {
        self.list.allocations + self.dense.allocations + self.cell.allocations
    }
    /// Total bytes requested from the allocator
    pub fn total_requested_bytes(&self) -> usize {
        self.list.requested_bytes + self.dense.requested_bytes + self.cell.requested_bytes
    }
    /// Total bytes after glibc chunk rounding
    pub fn total_malloc_bytes(&self) -> usize {
        self.list.malloc_bytes + self.dense.malloc_bytes + self.cell.malloc_bytes
    }
    /// Total bytes after jemalloc size-class rounding
    pub fn total_jemalloc_bytes(&self) -> usize {
        self.list.jemalloc_bytes + self.dense.jemalloc_bytes + self.cell.jemalloc_bytes
    }
    /// Key bytes materialised across the whole trie: the irreducible information the trie stores
    pub fn total_key_bytes(&self) -> usize {
        self.list.key_bytes + self.dense.key_bytes + self.cell.key_bytes
    }
    /// Total onward links
    pub fn total_child_links(&self) -> usize {
        self.list.child_links + self.dense.child_links + self.cell.child_links
    }
    /// Fraction of requested bytes that are key bytes
    pub fn key_byte_occupancy(&self) -> f64 {
        self.total_key_bytes() as f64 / self.total_requested_bytes().max(1) as f64
    }
    /// Fraction of the list nodes' key buffers that hold key bytes
    pub fn list_key_buffer_occupancy(&self) -> f64 {
        self.list.key_bytes as f64 / (self.list.nodes * KEY_BYTES_CNT).max(1) as f64
    }

    /// Projects the footprint under a set of candidate layout changes. Each projection is
    /// computed from the histograms gathered by [measure](Self::measure), so it reflects this
    /// trie's actual shape distribution rather than a synthetic model. Cell nodes are carried
    /// through unchanged.
    pub fn projections(&self) -> Vec<Projection> {
        let l = &self.layout;
        let val_align = if l.value == 0 { 1 } else { l.value.min(8) };
        let packed_entry = (8 + l.value + val_align - 1) & !(val_align - 1);
        let mut out = vec![];

        // Baseline, rebuilt from the histograms so the projections are comparable with it
        let baseline = self.project(|_len, cap| (l.dense_node, cap * l.dense_entry), |_len| 64, |_len| 64, |_shape| 64);
        out.push(Projection { name: "baseline (recomputed)".into(), note: "the measured layout, rebuilt from the histograms (matches the measured figures)".into(), ..baseline });

        // P1: one pointer-sized word per entry, has_val flag in the pointer's spare tag bits, V stored inline after it
        let p1 = self.project(|_len, cap| (l.dense_node, cap * packed_entry), |_len| 64, |_len| 64, |_shape| 64);
        out.push(Projection { name: "P1 packed cofree".into(), note: format!("dense entry {} B -> {} B (8 + size_of V)", l.dense_entry, packed_entry), ..p1 });

        // P2: never let the entries Vec grow past 256 and grow small nodes in +2 steps, so capacity == len for full nodes
        let clamp = |len: usize, cap: usize| if len >= 128 { cap.min(256) } else { cap };
        let p2 = self.project(|len, cap| (l.dense_node, clamp(len, cap) * l.dense_entry), |_len| 64, |_len| 64, |_shape| 64);
        out.push(Projection { name: "P2 cap clamp 256".into(), note: "Vec capacity never exceeds 256 entries".into(), ..p2 });

        // P3: leaf node of 24 B (4 refcnt + 1 header + 19 key bytes) for single-value ZST leaves
        let leaf24 = |len: usize| if l.value == 0 && len <= 19 { 24 } else { 64 };
        let p3 = self.project(|_len, cap| (l.dense_node, cap * l.dense_entry), leaf24, |_len| 64, |_shape| 64);
        out.push(Projection { name: "P3 24 B leaf node".into(), note: "Val-only list nodes with key <= 19 B become 24 B nodes (ZST values only)".into(), ..p3 });

        // P4: single-allocation 64 B sparse node for byte nodes with <= 6 entries (4 refcnt + 1 len + 6 keys + 6 x 8 B packed entries)
        let sparse = |len: usize, cap: usize| if len <= 6 { (64, 0) } else { (l.dense_node, cap * l.dense_entry) };
        let p4 = self.project(sparse, |_len| 64, |_len| 64, |_shape| 64);
        out.push(Projection { name: "P4 64 B sparse node".into(), note: "byte nodes with <= 6 entries become one 64 B block, no Vec".into(), ..p4 });

        // P5: chain nodes with 112 key bytes (128 B block) instead of 42. Full 42 B chain nodes are
        // fragments of longer paths, so they are re-packed at 42/112 nodes each; this ignores where
        // paths actually end, so it is an upper bound on the saving.
        let full_chains = self.list_chain_key_len_hist[KEY_BYTES_CNT];
        let repacked = (full_chains * KEY_BYTES_CNT + 111) / 112;
        let mut p5 = self.project(|_len, cap| (l.dense_node, cap * l.dense_entry), |_len| 64, |len| if len == KEY_BYTES_CNT { 0 } else { 64 }, |_shape| 64);
        p5.allocations += repacked;
        p5.requested_bytes += repacked * 128;
        p5.malloc_bytes += repacked * glibc_chunk_size(128);
        p5.jemalloc_bytes += repacked * jemalloc_size_class(128);
        out.push(Projection { name: "P5 128 B chain node".into(), note: "full 42 B single-child nodes re-packed into 112 B keys (upper bound)".into(), ..p5 });

        // P6: for a zero-sized V, a byte node whose entries are all values needs only its mask
        let mut p6 = self.project(|_len, cap| (l.dense_node, cap * l.dense_entry), |_len| 64, |_len| 64, |_shape| 64);
        if l.value == 0 && self.dense_val_only_nodes > 0 {
            // Remove the entries blocks of the all-value nodes. Their summed capacity is exact; the
            // allocator rounding is approximated with the mean block size.
            let removed = self.dense_val_only_capacity * l.dense_entry;
            let mean_block = removed / self.dense_val_only_nodes;
            p6.allocations -= self.dense_val_only_nodes;
            p6.requested_bytes -= removed;
            p6.malloc_bytes -= self.dense_val_only_nodes * glibc_chunk_size(mean_block);
            p6.jemalloc_bytes -= self.dense_val_only_nodes * jemalloc_size_class(mean_block);
        }
        out.push(Projection { name: "P6 mask-only leaf byte node".into(), note: format!("{} all-value byte nodes ({} entries) keep only their 64 B header (ZST values only)", self.dense_val_only_nodes, self.dense_val_only_entries), ..p6 });

        // Combined: P1 + P2 + P3 + P4 (P5 and P6 are reported separately: P5 is a bound, P6 overlaps P1/P2)
        let combined = self.project(
            |len, cap| if len <= 6 { (64, 0) } else { (l.dense_node, clamp(len, cap) * packed_entry) },
            leaf24, |_len| 64, |_shape| 64);
        out.push(Projection { name: "P1+P2+P3+P4".into(), note: "all of P1-P4".into(), ..combined });
        out
    }

    /// Evaluates a layout model over the gathered histograms. `dense(len, cap)` returns the
    /// (header block, entries block) sizes for a byte node; `leaf(key_len)` the block size of a
    /// single-value list node; `chain(key_len)` of a single-child list node; `other(shape)` of any
    /// other list node.
    fn project(&self, dense: impl Fn(usize, usize) -> (usize, usize), leaf: impl Fn(usize) -> usize, chain: impl Fn(usize) -> usize, other: impl Fn(ListShape) -> usize) -> Projection {
        let mut p = Projection::default();
        let mut add = |size: usize| {
            if size > 0 {
                p.allocations += 1;
                p.requested_bytes += size;
                p.malloc_bytes += glibc_chunk_size(size);
                p.jemalloc_bytes += jemalloc_size_class(size);
            }
        };
        for (len, n) in self.list_leaf_key_len_hist.iter().enumerate() {
            for _ in 0..*n { add(leaf(len)); }
        }
        for (len, n) in self.list_chain_key_len_hist.iter().enumerate() {
            for _ in 0..*n { add(chain(len)); }
        }
        for shape in ListShape::ALL {
            if matches!(shape, ListShape::Val | ListShape::Child) { continue; }
            for _ in 0..self.list_shape_hist[shape.index()] { add(other(shape)); }
        }
        for (&(len, cap), n) in self.dense_len_cap_hist.iter() {
            for _ in 0..*n {
                let (header, entries) = dense(len, cap);
                add(header);
                add(entries);
            }
        }
        // Cell nodes pass through unchanged
        p.allocations += self.cell.allocations;
        p.requested_bytes += self.cell.requested_bytes;
        p.malloc_bytes += self.cell.malloc_bytes;
        p.jemalloc_bytes += self.cell.jemalloc_bytes;
        p
    }

    /// Renders the [projections](Self::projections) as a table
    pub fn projection_report(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let projections = self.projections();
        let base = &projections[0];
        let _ = writeln!(out, "{:<24} {:>10} {:>12} {:>7} {:>12} {:>7} {:>12} {:>7}  note", "projection", "allocs", "requested", "vs base", "glibc_est", "vs base", "jemalloc_est", "vs base");
        for p in &projections {
            let ratio = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
            let _ = writeln!(out, "{:<24} {:>10} {:>12} {:>6.1}% {:>12} {:>6.1}% {:>12} {:>6.1}%  {}",
                p.name, p.allocations, p.requested_bytes, ratio(p.requested_bytes, base.requested_bytes),
                p.malloc_bytes, ratio(p.malloc_bytes, base.malloc_bytes), p.jemalloc_bytes, ratio(p.jemalloc_bytes, base.jemalloc_bytes), p.note);
        }
        out
    }

    /// Renders a multi-section text report
    pub fn report(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let l = &self.layout;
        let _ = writeln!(out, "layout: V={} B  node ptr={} B  list node={} B ({} key bytes)  dense node={} B + {} B/entry  cell node={} B + {} B/entry (+{} B boxed)",
            l.value, l.node_ptr, l.list_node, l.list_key_bytes, l.dense_node, l.dense_entry, l.cell_node, l.cell_entry, l.dense_entry);
        let pct = |num: usize, den: usize| if den == 0 { 0.0 } else { 100.0 * num as f64 / den as f64 };
        let per_val = |num: usize| if self.value_count == 0 { 0.0 } else { num as f64 / self.value_count as f64 };

        let _ = writeln!(out, "values: {}  (size_of V = {})   key bytes: {}   child links: {}   nodes: {}   shared nodes: {} (+{} extra refs)",
            self.value_count, self.value_size, self.total_key_bytes(), self.total_child_links(), self.total_nodes(), self.shared_nodes, self.shared_extra_refs);
        let _ = writeln!(out, "allocations: {}   requested: {} B   glibc est: {} B   jemalloc est: {} B   bytes/value: {:.1} req, {:.1} glibc   key-byte occupancy: {:.1}%",
            self.total_allocations(), self.total_requested_bytes(), self.total_malloc_bytes(), self.total_jemalloc_bytes(),
            per_val(self.total_requested_bytes()), per_val(self.total_malloc_bytes()), 100.0 * self.key_byte_occupancy());
        let _ = writeln!(out, "{:<6} {:>9} {:>9} {:>6} {:>12} {:>6} {:>12} {:>10} {:>9} {:>8} {:>9}",
            "kind", "nodes", "allocs", "%nodes", "req_bytes", "%req", "malloc_bytes", "key_bytes", "children", "values", "B/keybyte");
        for (name, k) in [("list", &self.list), ("dense", &self.dense), ("cell", &self.cell)] {
            if k.nodes == 0 { continue; }
            let _ = writeln!(out, "{:<6} {:>9} {:>9} {:>5.1}% {:>12} {:>5.1}% {:>12} {:>10} {:>9} {:>8} {:>9.1}",
                name, k.nodes, k.allocations, pct(k.nodes, self.total_nodes()), k.requested_bytes, pct(k.requested_bytes, self.total_requested_bytes()),
                k.malloc_bytes, k.key_bytes, k.child_links, k.values, k.requested_bytes as f64 / k.key_bytes.max(1) as f64);
        }

        if self.list.nodes > 0 {
            let _ = writeln!(out, "list nodes: key buffer occupancy {:.1}% ({} of {} B)   chain (single child): {} ({:.1}%)   leaf (single value): {} ({:.1}%)",
                100.0 * self.list_key_buffer_occupancy(), self.list.key_bytes, self.list.nodes * KEY_BYTES_CNT,
                self.list_chain_nodes, pct(self.list_chain_nodes, self.list.nodes), self.list_leaf_nodes, pct(self.list_leaf_nodes, self.list.nodes));
            let _ = write!(out, "  shape:");
            for shape in ListShape::ALL {
                let n = self.list_shape_hist[shape.index()];
                if n > 0 {
                    let _ = write!(out, "  {:?}={} ({:.1}%)", shape, n, pct(n, self.list.nodes));
                }
            }
            let _ = writeln!(out);
            let _ = write!(out, "  leaf key_len cumulative:");
            let mut cumulative = 0;
            for (len, n) in self.list_leaf_key_len_hist.iter().enumerate() {
                if *n > 0 {
                    cumulative += n;
                    let _ = write!(out, " <={}:{:.0}%", len, pct(cumulative, self.list_leaf_nodes));
                }
            }
            let _ = writeln!(out);
            let _ = write!(out, "  slot key_len:");
            let slots: usize = self.list_key_len_hist.iter().sum();
            let mut cumulative = 0;
            for (len, n) in self.list_key_len_hist.iter().enumerate() {
                if *n > 0 {
                    cumulative += n;
                    let _ = write!(out, " {}:{} ({:.0}%)", len, n, pct(cumulative, slots));
                }
            }
            let _ = writeln!(out);
        }
        let dense_nodes = self.dense.nodes + self.cell.nodes;
        if dense_nodes > 0 {
            let s = &self.dense_slots;
            let _ = writeln!(out, "byte nodes: entries {} in capacity {} ({:.1}% used, {} nodes over-allocated)   rec_only={} val_only={} both={}",
                s.len, s.capacity, pct(s.len, s.capacity), self.dense_overallocated_nodes, s.rec_only, s.val_only, s.both);
            let _ = write!(out, "  entries/node:");
            for (b, n) in self.dense_items_hist.iter().enumerate() {
                if *n > 0 {
                    let _ = write!(out, "  {}:{} ({:.1}%)", dense_bucket_label(b), n, pct(*n, dense_nodes));
                }
            }
            let _ = writeln!(out);
            let _ = write!(out, "  exact entries (<=8):");
            for len in 1..=8 {
                let _ = write!(out, " {}:{}", len, self.dense_items_exact[len]);
            }
            let _ = writeln!(out, "   256:{}", self.dense_items_exact[256]);
            let _ = writeln!(out, "  all-value byte nodes: {} ({:.1}%) holding {} entries in capacity {}", self.dense_val_only_nodes, pct(self.dense_val_only_nodes, dense_nodes), self.dense_val_only_entries, self.dense_val_only_capacity);
        }
        out
    }
}

/// Footprint of a trie under a hypothetical node layout, see [SizeStats::projections]
#[derive(Clone, Debug, Default)]
pub struct Projection {
    /// Short name of the layout change
    pub name: String,
    /// What the change assumes
    pub note: String,
    /// Heap blocks
    pub allocations: usize,
    /// Bytes requested from the allocator
    pub requested_bytes: usize,
    /// Bytes after glibc chunk rounding
    pub malloc_bytes: usize,
    /// Bytes after jemalloc size-class rounding
    pub jemalloc_bytes: usize,
}

/// Pushes every onward link of `node` onto `stack`. Dispatches on the concrete node type so the
/// borrowed children carry the trie's lifetime rather than the lifetime of the local `TaggedNodeRef`.
fn push_children<'a, V: Clone + Send + Sync, A: Allocator>(node: &TaggedNodeRef<'a, V, A>, stack: &mut Vec<&'a TrieNodeODRc<V, A>>) {
    fn collect<'a, V: Clone + Send + Sync, A: Allocator, N: TrieNode<V, A> + ?Sized>(node: &'a N, stack: &mut Vec<&'a TrieNodeODRc<V, A>>) {
        let (mut token, mut child) = node.node_child_iter_start();
        while let Some(c) = child {
            stack.push(c);
            (token, child) = node.node_child_iter_next(token);
        }
    }
    match node {
        TaggedNodeRef::LineListNode(n) => collect(*n, stack),
        TaggedNodeRef::DenseByteNode(n) => collect(*n, stack),
        TaggedNodeRef::CellByteNode(n) => collect(*n, stack),
        #[cfg(feature = "bridge_nodes")]
        TaggedNodeRef::BridgeNode(n) => collect(*n, stack),
        TaggedNodeRef::TinyRefNode(_) | TaggedNodeRef::EmptyNode => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zipper::*;

    #[test]
    fn counts_match_the_map() {
        let mut map: PathMap<()> = PathMap::new();
        let keys: Vec<Vec<u8>> = (0u32..5000).map(|i| format!("k{}/{}", i % 37, i).into_bytes()).collect();
        for k in &keys {
            map.set_val_at(k, ());
        }
        let stats = SizeStats::measure(&map);
        assert_eq!(stats.value_count, map.val_count());
        assert_eq!(stats.total_nodes(), stats.total_child_links() + 1, "every non-root node is reached by exactly one link when nothing is shared");
        assert_eq!(stats.shared_nodes, 0);
        assert!(stats.list.nodes > 0 && stats.dense.nodes > 0);
        assert_eq!(stats.list_shape_hist.iter().sum::<usize>(), stats.list.nodes);
        assert_eq!(stats.list_leaf_key_len_hist.iter().sum::<usize>(), stats.list_leaf_nodes);
        assert_eq!(stats.dense_items_hist.iter().sum::<usize>(), stats.dense.nodes);
        assert!(stats.dense_slots.capacity >= stats.dense_slots.len);
        assert!(stats.total_requested_bytes() >= stats.total_nodes() * 64);
        // The projection machinery must reproduce the measured baseline exactly
        let base = &stats.projections()[0];
        assert_eq!(base.allocations, stats.total_allocations());
        assert_eq!(base.requested_bytes, stats.total_requested_bytes());
        assert_eq!(base.malloc_bytes, stats.total_malloc_bytes());
        let _ = stats.report();
        let _ = stats.projection_report();
    }

    #[test]
    fn shared_subtries_are_counted_once() {
        let mut sub: PathMap<()> = PathMap::new();
        for i in 0..300u32 {
            sub.set_val_at(i.to_be_bytes(), ());
        }
        let alone = SizeStats::measure(&sub);
        let mut map: PathMap<()> = PathMap::new();
        map.write_zipper_at_path(b"a").graft_map(sub.clone());
        map.write_zipper_at_path(b"b").graft_map(sub.clone());
        let stats = SizeStats::measure(&map);
        assert_eq!(map.val_count(), 600);
        assert_eq!(stats.value_count, 300, "the shared subtrie's values live in memory once");
        assert!(stats.shared_nodes >= 1);
        assert_eq!(stats.shared_extra_refs, stats.shared_nodes);
        assert!(stats.total_nodes() < 2 * alone.total_nodes());
    }

    #[test]
    fn full_byte_node_capacity_is_exact() {
        let mut map: PathMap<()> = PathMap::new();
        for b in 0..=255u8 {
            map.set_val_at([b], ());
        }
        let stats = SizeStats::measure(&map);
        assert_eq!(stats.dense.nodes, 1);
        assert_eq!(stats.dense_slots.len, 256);
        assert_eq!(stats.dense_slots.capacity, 256, "the entries Vec must not overshoot the 256 possible entries");
    }

    #[test]
    fn allocator_rounding() {
        assert_eq!(glibc_chunk_size(64), 80);
        assert_eq!(glibc_chunk_size(24), 32);
        assert_eq!(glibc_chunk_size(48), 64);
        assert_eq!(jemalloc_size_class(64), 64);
        assert_eq!(jemalloc_size_class(65), 80);
        assert_eq!(jemalloc_size_class(129), 160);
        assert_eq!(jemalloc_size_class(6144), 6144);
        assert_eq!(jemalloc_size_class(6145), 7168);
        assert_eq!(dense_bucket(1), 0);
        assert_eq!(dense_bucket(2), 1);
        assert_eq!(dense_bucket(3), 2);
        assert_eq!(dense_bucket(4), 2);
        assert_eq!(dense_bucket(5), 3);
        assert_eq!(dense_bucket(256), 8);
    }
}
