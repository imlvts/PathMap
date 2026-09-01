//! Regression repro for a pre-existing LineListNode invariant violation: grafting a subtrie at a
//! path that is a strict prefix of an existing value's key (here b"abe/tw" below the value at
//! b"abe/twelve_bytes") makes `set_payload_abstract` produce a node with an onward link in slot 0
//! whose key is a prefix of the value key in slot 1, which `validate_node` rejects
//! ("ambiguous path violation").  It reproduces on `master` (2192ebc) with plain `usize` values,
//! independently of the leaf node types.  Ignored until the list node insertion is fixed.
use pathmap::PathMap;
use pathmap::zipper::*;

#[test]
#[ignore = "pre-existing LineListNode ambiguous-path violation on graft below a value prefix"]
fn graft_below_value_prefix() {
    let mut map: PathMap<usize> = PathMap::new();
    map.set_val_at(b"abc/twelve_bytes", 1);
    map.set_val_at(b"abd/twelve_bytes", 2);
    map.set_val_at(b"abe/twelve_bytes", 3);
    let mut other: PathMap<usize> = PathMap::new();
    other.set_val_at(b"abc/twelve_bytes", 4);
    other.set_val_at(b"abz/another_long_tail", 5);
    map.write_zipper_at_path(b"abe/tw").graft_map(other.clone());
    assert!(map.contains(b"abe/twabc/twelve_bytes"));
    assert!(!map.contains(b"abe/twelve_bytes"));
}
