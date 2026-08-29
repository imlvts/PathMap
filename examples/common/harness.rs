// Shared fuzzing harness for `pathmap`'s zipper API.
//
// One decoder, used by two front ends:
//
// * `examples/pathmap_trace.rs` prints a trace, which `lean/differential.py`
//   diffs against the Lean model's trace for the same bytes.
// * `fuzz/fuzz_targets/zipper_ops.rs` runs the same program under libFuzzer and
//   checks structural invariants in-process (no oracle needed, so it is fast).
//
// The wire format and operation table are a contract shared with
// `lean/PathMapModel/Fuzz.lean`; any change here must be mirrored there.
//
// Included with `include!`, not `mod`, because the two front ends live in
// different crates.


/// Number of distinct operations. Must match `PathMapModel.Fuzz.nops`.
const NOPS: usize = 47;
/// Maximum operations executed. Must match the `maxSteps` default in `Fuzz.run`.
const MAX_STEPS: usize = 256;
/// Maximum entries in a `dump`. Must match `Fuzz.dumpAt`.
const DUMP_CAP: usize = 64;

struct Dec<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn modn(&mut self, m: usize) -> Option<usize> {
        let b = self.u8()?;
        Some(if m == 0 { 0 } else { (b as usize) % m })
    }
    /// Path bytes live in a 4-letter alphabet so generated tries share prefixes.
    fn path_byte(&mut self) -> Option<u8> {
        Some(self.u8()? % 4)
    }
    fn path_n(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.path_byte()?);
        }
        Some(v)
    }
    fn path(&mut self, lim: usize) -> Option<Vec<u8>> {
        let n = self.modn(lim)?;
        self.path_n(n)
    }
    fn boolean(&mut self) -> Option<bool> {
        Some(self.u8()? % 2 == 1)
    }
}

fn hex_path(p: &[u8]) -> String {
    if p.is_empty() {
        "_".to_string()
    } else {
        p.iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn show_val(v: Option<&u64>) -> String {
    match v {
        None => "-".to_string(),
        Some(v) => format!("{v}"),
    }
}

fn show_status(s: AlgebraicStatus) -> &'static str {
    match s {
        AlgebraicStatus::Element => "Element",
        AlgebraicStatus::Identity => "Identity",
        AlgebraicStatus::None => "None",
    }
}

fn show_bool(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

/// Per-step fingerprint of a zipper.
///
/// `expected_root` is the root the zipper was created at.  A zipper must never
/// leave it, so a mismatch is tagged `ESCAPED-ROOT` — the model can never
/// produce that tag, which lets `differential.py` recognise the escape bug
/// instead of reporting it as an unexplained state divergence.
fn fingerprint<Z: ZipperMoving + ZipperValues<u64> + ZipperAbsolutePath>(
    z: &Z,
    expected_root: &[u8],
) -> String {
    format!(
        "{}{} o{} e{} v{} c{} n{}",
        if z.root_prefix_path() == expected_root { "" } else { "ESCAPED-ROOT " },
        hex_path(z.path()),
        hex_path(z.origin_path()),
        show_bool(z.path_exists()),
        show_val(z.val()),
        z.child_count(),
        z.val_count()
    )
}

/// Does the focus have no descendants at all?
///
/// Several return values (`remove_branches`, `restricting`, `join_map_into`,
/// `take_map`, `restrict`) hinge on whether an *empty node* happens to be
/// materialised at the focus rather than on the logical state, so the harness
/// masks them here.  See lean/README.md.
fn focus_node_empty<Z: Zipper>(z: &Z) -> bool {
    z.child_count() == 0
}

/// Depth-first dump of everything at and below the zipper's root, relative to it.
fn dump<Z: ZipperMoving + ZipperValues<u64>>(z: &mut Z) -> String {
    z.reset();
    let mut out = vec![format!("{}:{}", hex_path(z.path()), show_val(z.val()))];
    while out.len() < DUMP_CAP && z.to_next_step() {
        // A depth-first step from the root must go deeper.  Coming back to the
        // empty path means the zipper walked out of its own root -- the escape
        // in lean/FINDINGS.md #3, which would otherwise show up here as a
        // traversal that visits the same location repeatedly.
        if z.path().is_empty() {
            out.push("ESCAPED-ROOT".to_string());
            break;
        }
        out.push(format!("{}:{}", hex_path(z.path()), show_val(z.val())));
    }
    out.join(",")
}

/// Bind `$z` to the write zipper (`t == 0`) or the read zipper, then run `$e`.
macro_rules! tgt {
    ($t:expr, $wz:expr, $rz:expr, $z:ident, $e:expr) => {
        if $t == 0 {
            let $z = &mut $wz;
            $e
        } else {
            let $z = &mut $rz;
            $e
        }
    };
}


/// Structural invariants every zipper must satisfy at every moment.
///
/// These need no oracle, so the libFuzzer target can check them on every step.
/// Violations are `pathmap` bugs by construction: each is either stated in the
/// trait documentation or forced by the meaning of the accessors.
fn check_zipper<Z>(z: &Z, label: &str, expected_root: &[u8])
where
    Z: ZipperMoving + ZipperValues<u64> + ZipperAbsolutePath,
{
    // `at_root` is defined as "the path back to the root is empty".
    assert_eq!(
        z.at_root(),
        z.path().is_empty(),
        "{label}: at_root disagrees with path()"
    );
    // Documented on `ZipperAbsolutePath`: origin = root_prefix ++ path.
    let mut want = z.root_prefix_path().to_vec();
    want.extend_from_slice(z.path());
    assert_eq!(
        z.origin_path(),
        &want[..],
        "{label}: origin_path != root_prefix_path ++ path"
    );
    // A zipper may never leave the subtrie it was granted.
    assert_eq!(
        z.root_prefix_path(),
        expected_root,
        "{label}: the zipper escaped its own root"
    );
    // A value can only sit on a path that exists.
    if z.is_val() {
        assert!(z.path_exists(), "{label}: value on a non-existent path");
    }
    // `child_count` is the population count of `child_mask`.
    assert_eq!(
        z.child_count(),
        z.child_mask().iter().count(),
        "{label}: child_count != |child_mask|"
    );
    // Children imply the parent exists.
    if z.child_count() > 0 {
        assert!(z.path_exists(), "{label}: children under a non-existent path");
    }
    // `val_count` counts the focus itself.
    if z.is_val() {
        assert!(z.val_count() >= 1, "{label}: val_count omits the focus value");
    }
    // Nothing below means nothing to count except the focus.
    if z.child_count() == 0 {
        assert_eq!(
            z.val_count(),
            z.is_val() as usize,
            "{label}: val_count on a leaf"
        );
    }
}
/// Decode and execute a fuzzer input.
///
/// With `check`, every zipper is validated against `check_zipper` after every
/// operation; that is the mode the libFuzzer target uses.  The trace tool runs
/// with `check == false`, because it is comparing against the model rather than
/// asserting, and a crate that violates an invariant should show up as a trace
/// diff rather than as an abort.
fn run(bytes: &[u8], check: bool) -> Vec<String> {
    let mut d = Dec { bytes, pos: 0 };
    let mut out: Vec<String> = Vec::new();

    // ---- header ----
    let header = (|| -> Option<(PathMap<u64>, PathMap<u64>, Vec<u8>, Vec<u8>)> {
        let mut m0 = PathMap::<u64>::new();
        let n0 = d.modn(8)?;
        for _ in 0..n0 {
            let p = d.path(6)?;
            let v = d.u8()? as u64;
            let mut wz = m0.write_zipper_at_path(&p);
            wz.set_val(v);
            drop(wz);
        }
        let mut m1 = PathMap::<u64>::new();
        let n1 = d.modn(8)?;
        for _ in 0..n1 {
            let p = d.path(6)?;
            let v = d.u8()? as u64;
            let mut wz = m1.write_zipper_at_path(&p);
            wz.set_val(v);
            drop(wz);
        }
        let r0 = d.path(4)?;
        let r1 = d.path(4)?;
        // Both zipper roots are created if absent: a zipper whose root does not
        // exist can walk out of it (see `Zip.toNextSiblingByte`), which would
        // contaminate every other comparison.
        if !r0.is_empty() { m0.create_path(&r0); }
        if !r1.is_empty() { m1.create_path(&r1); }
        Some((m0, m1, r0, r1))
    })();

    let (mut map0, map1, root0, root1) = match header {
        Some(x) => x,
        None => return vec!["EMPTY".to_string()],
    };

    {
        let mut wz = map0.write_zipper_at_path(&root0);
        // NOTE: `read_zipper_at_borrowed_path` would panic (release: wrap) in
        // `to_next_k_path`, whose `path_len()` underflows before the path buffer
        // is prepared.  Use the owned-path constructor so that known bug does
        // not abort every run.
        let mut rz = map1.read_zipper_at_path(&root1);
        let mut step = 0usize;
        // Explicit pruning is only well-defined for a zipper at the map root;
        // off it the depth pruned depends on internal node layout.
        let pruneable = root0.is_empty();
        // The `prune` flag on the other operations is passed straight to
        // `node_remove_*`, which prunes within the node even when it finds
        // nothing and reports `None` -- so its effect is a function of node
        // layout, not of the trie.  Always false.  See lean/FINDINGS.md #7.
        let no_prune = false;

        macro_rules! get {
            ($e:expr) => {
                match $e {
                    Some(x) => x,
                    None => break,
                }
            };
        }

        loop {
            if step >= MAX_STEPS {
                break;
            }
            let op = get!(d.u8()) as usize % NOPS;
            let (name, ret): (&str, String) = match op {
                0 => {
                    let t = get!(d.modn(2));
                    let p = get!(d.path(6));
                    tgt!(t, wz, rz, z, z.descend_to(&p));
                    ("descend_to", hex_path(&p))
                }
                1 => {
                    let t = get!(d.modn(2));
                    let b = get!(d.path_byte());
                    tgt!(t, wz, rz, z, z.descend_to_byte(b));
                    ("descend_to_byte", format!("{b:02x}"))
                }
                2 => {
                    let t = get!(d.modn(2));
                    let n = get!(d.modn(8));
                    let r = tgt!(t, wz, rz, z, z.ascend(n));
                    ("ascend", show_bool(r).to_string())
                }
                3 => {
                    let t = get!(d.modn(2));
                    let r = tgt!(t, wz, rz, z, z.ascend_byte());
                    ("ascend_byte", show_bool(r).to_string())
                }
                4 => {
                    let t = get!(d.modn(2));
                    tgt!(t, wz, rz, z, z.reset());
                    ("reset", "-".to_string())
                }
                5 => {
                    let t = get!(d.modn(2));
                    let r = tgt!(t, wz, rz, z, z.descend_first_byte());
                    ("descend_first_byte", show_bool(r).to_string())
                }
                6 => {
                    let t = get!(d.modn(2));
                    let r = tgt!(t, wz, rz, z, z.descend_last_byte());
                    ("descend_last_byte", show_bool(r).to_string())
                }
                7 => {
                    let t = get!(d.modn(2));
                    let i = get!(d.modn(6));
                    let r = tgt!(t, wz, rz, z, z.descend_indexed_byte(i));
                    ("descend_indexed_byte", show_bool(r).to_string())
                }
                8 => {
                    let t = get!(d.modn(2));
                    let r = tgt!(t, wz, rz, z, z.descend_until());
                    ("descend_until", show_bool(r).to_string())
                }
                9 => {
                    let t = get!(d.modn(2));
                    let r = tgt!(t, wz, rz, z, z.ascend_until());
                    ("ascend_until", show_bool(r).to_string())
                }
                10 => {
                    let t = get!(d.modn(2));
                    let r = tgt!(t, wz, rz, z, z.ascend_until_branch());
                    ("ascend_until_branch", show_bool(r).to_string())
                }
                11 => {
                    let t = get!(d.modn(2));
                    // Skipped at the zipper root: the native ReadZipper escapes
                    // its own root there. See `Zip.toNextSiblingByte`.
                    if tgt!(t, wz, rz, z, z.at_root()) {
                        ("to_next_sibling_byte", "skip".to_string())
                    } else {
                        let r = tgt!(t, wz, rz, z, z.to_next_sibling_byte());
                        ("to_next_sibling_byte", show_bool(r).to_string())
                    }
                }
                12 => {
                    let t = get!(d.modn(2));
                    if tgt!(t, wz, rz, z, z.at_root()) {
                        ("to_prev_sibling_byte", "skip".to_string())
                    } else {
                        let r = tgt!(t, wz, rz, z, z.to_prev_sibling_byte());
                        ("to_prev_sibling_byte", show_bool(r).to_string())
                    }
                }
                13 => {
                    let t = get!(d.modn(2));
                    let r = tgt!(t, wz, rz, z, z.to_next_step());
                    ("to_next_step", show_bool(r).to_string())
                }
                14 => {
                    // `ZipperIteration` is read-only: the target byte is still
                    // consumed, but the operation always applies to `rz`.
                    let _t = get!(d.modn(2));
                    let r = rz.to_next_val();
                    ("to_next_val", show_bool(r).to_string())
                }
                15 => {
                    let _t = get!(d.modn(2));
                    let k = get!(d.modn(4));
                    // k == 0 is degenerate; see Fuzz.lean.
                    if k == 0 {
                        ("descend_first_k_path", "skip".to_string())
                    } else {
                        let r = rz.descend_first_k_path(k);
                        ("descend_first_k_path", show_bool(r).to_string())
                    }
                }
                16 => {
                    let _t = get!(d.modn(2));
                    let k = get!(d.modn(4));
                    // A whole k-path iteration: `to_next_k_path` on its own is
                    // unspecified (it continues state left by
                    // `descend_first_k_path`).
                    let mut v: Vec<String> = Vec::new();
                    if k == 0 {
                        out.push(format!(
                            "{step} k_path_walk ret=skip W={} R={}",
                            fingerprint(&wz, &root0), fingerprint(&rz, &root1)));
                        step += 1;
                        continue;
                    }
                    if rz.descend_first_k_path(k) {
                        v.push(hex_path(rz.path()));
                        while v.len() < 32 && rz.to_next_k_path(k) {
                            v.push(hex_path(rz.path()));
                        }
                    }
                    ("k_path_walk", v.join(","))
                }
                17 => {
                    let _t = get!(d.modn(2));
                    let r = rz.descend_last_path();
                    ("descend_last_path", show_bool(r).to_string())
                }
                18 => {
                    let t = get!(d.modn(2));
                    let p = get!(d.path(6));
                    let n = tgt!(t, wz, rz, z, z.move_to_path(&p));
                    ("move_to_path", format!("{n}"))
                }
                19 => {
                    let t = get!(d.modn(2));
                    let p = get!(d.path(6));
                    let n = tgt!(t, wz, rz, z, z.descend_to_existing(&p));
                    ("descend_to_existing", format!("{n}"))
                }
                20 => {
                    let t = get!(d.modn(2));
                    let p = get!(d.path(6));
                    let n = tgt!(t, wz, rz, z, z.descend_to_val(&p));
                    ("descend_to_val", format!("{n}"))
                }
                21 => {
                    let t = get!(d.modn(2));
                    let b = get!(d.path_byte());
                    let r = tgt!(t, wz, rz, z, z.descend_to_existing_byte(b));
                    ("descend_to_existing_byte", show_bool(r).to_string())
                }
                22 => {
                    let t = get!(d.modn(2));
                    let n = get!(d.modn(8));
                    let r = tgt!(t, wz, rz, z, z.descend_until_max_bytes(n));
                    ("descend_until_max_bytes", show_bool(r).to_string())
                }
                23 => {
                    let t = get!(d.modn(2));
                    let p = get!(d.path(6));
                    let r = tgt!(t, wz, rz, z, z.descend_to_check(&p));
                    ("descend_to_check", show_bool(r).to_string())
                }
                24 => {
                    let t = get!(d.modn(2));
                    let p = get!(d.path(6));
                    let v = tgt!(t, wz, rz, z, show_val(z.val_at(&p)));
                    ("val_at", v)
                }
                25 => {
                    let t = get!(d.modn(2));
                    let n = tgt!(t, wz, rz, z, z.make_map().val_count());
                    ("make_map_val_count", format!("{n}"))
                }
                26 => {
                    let t = get!(d.modn(2));
                    let s = tgt!(t, wz, rz, z, dump(&mut z.fork_read_zipper()));
                    ("dump", s)
                }
                27 => {
                    let v = get!(d.u8()) as u64;
                    ("set_val", show_val(wz.set_val(v).as_ref()))
                }
                28 => {
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    ("remove_val", show_val(wz.remove_val(no_prune).as_ref()))
                }
                29 => ("create_path", show_bool(wz.create_path()).to_string()),
                30 => {
                    if pruneable {
                        ("prune_path", format!("{}", wz.prune_path()))
                    } else {
                        ("prune_path", "skip".to_string())
                    }
                }
                31 => {
                    if pruneable {
                        ("prune_ascend", format!("{}", wz.prune_ascend()))
                    } else {
                        ("prune_ascend", "skip".to_string())
                    }
                }
                32 => {
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    let leaky = focus_node_empty(&wz);
                    let r = wz.remove_branches(no_prune);
                    let s = if leaky { "?".to_string() } else { show_bool(r).to_string() };
                    ("remove_branches", s)
                }
                33 => {
                    let n = get!(d.modn(4));
                    let m = get!(d.path_n(n));
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    let mask = ByteMask::from_iter(m.iter().copied());
                    wz.remove_unmasked_branches(mask, no_prune);
                    let mut canon: Vec<u8> = m.clone();
                    canon.sort_unstable();
                    canon.dedup();
                    ("remove_unmasked_branches", hex_path(&canon))
                }
                34 => {
                    wz.graft(&rz);
                    ("graft", "-".to_string())
                }
                35 => {
                    let p = get!(d.path(6));
                    wz.graft_src_at(&rz, &p);
                    ("graft_src_at", hex_path(&p))
                }
                36 => ("join_into", show_status(wz.join_into(&rz)).to_string()),
                37 => {
                    let leaky = focus_node_empty(&wz);
                    let st = wz.join_map_into(rz.make_map());
                    let s = if leaky { "?".to_string() } else { show_status(st).to_string() };
                    ("join_map_into", s)
                }
                38 => {
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    ("meet_into", show_status(wz.meet_into(&rz, no_prune)).to_string())
                }
                39 => {
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    (
                        "subtract_into",
                        show_status(wz.subtract_into(&rz, no_prune)).to_string(),
                    )
                }
                40 => {
                    let leaky = focus_node_empty(&wz);
                    let st = wz.restrict(&rz);
                    let s = if leaky { "?".to_string() } else { show_status(st).to_string() };
                    ("restrict", s)
                }
                41 => {
                    // Skipped when either side has nothing below its focus; see
                    // Fuzz.lean and lean/FINDINGS.md #8.
                    if focus_node_empty(&wz) || focus_node_empty(&rz) {
                        ("restricting", "skip".to_string())
                    } else {
                        ("restricting", show_bool(wz.restricting(&rz)).to_string())
                    }
                }
                42 => {
                    let k = get!(d.modn(4));
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    // `join_k_path_into(0)` destroys the subtrie in pathmap 0.3.1.
                    if k == 0 {
                        ("join_k_path_into", "skip".to_string())
                    } else {
                        // The bool leaks node materialisation; see FINDINGS.md #8.
                        let r = wz.join_k_path_into(k, no_prune);
                        let s = if focus_node_empty(&wz) {
                            "?".to_string()
                        } else {
                            show_bool(r).to_string()
                        };
                        ("join_k_path_into", s)
                    }
                }
                43 => {
                    let p = get!(d.path(6));
                    // `insert_prefix("")` destroys the subtrie in pathmap 0.3.1.
                    if p.is_empty() {
                        ("insert_prefix", "skip".to_string())
                    } else {
                        ("insert_prefix", show_bool(wz.insert_prefix(&p)).to_string())
                    }
                }
                44 => {
                    let n = get!(d.modn(6));
                    ("remove_prefix", show_bool(wz.remove_prefix(n)).to_string())
                }
                45 => {
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    let leaky = focus_node_empty(&wz) && wz.val().is_none();
                    let r = match wz.take_map(no_prune) {
                        Some(m) => {
                            wz.graft_map(m);
                            "1"
                        }
                        None => "0",
                    };
                    ("take_map_restore", if leaky { "?".to_string() } else { r.to_string() })
                }
                46 => {
                    let k = get!(d.modn(4));
                    let _pr = get!(d.boolean()); // decoded for stream alignment; see `no_prune`
                    // `meet_k_path_into` spins forever when the focus has no
                    // children, and escapes the focus subtree when k == 0.
                    // See `Zip.meetKPathUnspecified`.
                    if k == 0 || wz.child_count() == 0 {
                        ("meet_k_path_into", "skip".to_string())
                    } else {
                        (
                            "meet_k_path_into",
                            show_bool(wz.meet_k_path_into(k, no_prune)).to_string(),
                        )
                    }
                }
                _ => ("nop", "-".to_string()),
            };
            if check {
                check_zipper(&wz, "write zipper", &root0);
                check_zipper(&rz, "read zipper", &root1);
            }
            out.push(format!(
                "{step} {name} ret={ret} W={} R={}",
                fingerprint(&wz, &root0),
                fingerprint(&rz, &root1)
            ));
            step += 1;
        }
    }

    out.push(format!("MAP0 {}", dump(&mut map0.read_zipper())));
    out.push(format!("MAP1 {}", dump(&mut map1.read_zipper())));
    out.push(format!("ROOT0 {}", hex_path(&root0)));
    out.push(format!("ROOT1 {}", hex_path(&root1)));
    out
}
