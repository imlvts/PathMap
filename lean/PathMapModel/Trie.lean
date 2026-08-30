import PathMapModel.Basic

/-!
# The trie / `PathMap` model

## Representation

`pathmap` tries are radix-256 tries in which a location can exist *without*
carrying a value: `create_path` makes such a **dangling path**, and
`remove_val(false)` leaves one behind.  Consequently the observable state of a
trie is **two** finite objects, not one:

* `vals`  — the finite partial map from paths to values, and
* `paths` — the finite, prefix-closed set of locations that "exist", i.e. the
            set on which `Zipper::path_exists` returns `true`.

`Trie` stores exactly those two components in canonical form (sorted, duplicate
free, `paths` prefix-closed and containing every key of `vals`).  Because the
form is canonical, structural equality of `Trie`s *is* observational equality —
which is what lets the model decide `AlgebraicStatus::Identity` vs `Element`.

A `pathmap::PathMap` is a `Trie` including the value at the empty path (its
*root value*).  A `pathmap` **node** — what `get_focus` returns and what
`graft_internal` replaces — is everything *strictly below* a location; the
value at the location itself lives in the parent's cell.  The model keeps that
distinction explicit: `Trie.subtrie` includes the focus value, `Trie.below`
does not, and `Trie.graftBelow` only ever rewrites the strictly-below part.
-/

namespace PathMapModel

/-- A `pathmap` trie: a finite path→value map plus the prefix-closed set of
existing locations.  Canonical: both fields sorted and duplicate-free, `paths`
prefix-closed and a superset of the keys of `vals`, and `[] ∈ paths`. -/
structure Trie (V : Type) where
  /-- Every location that exists, in depth-first order, each carrying its value
  if it has one.  Canonical: sorted by path, no duplicate paths, prefix-closed,
  and containing `[]`.

  One list rather than a path→value map beside a set of existing paths, because
  a value cannot then be recorded at a location that does not exist -- the
  invariant is structural instead of something the constructors have to
  maintain.  A `none` here is exactly a dangling path. -/
  entries : List (Path × Option V)
deriving Repr

namespace Trie

variable {V : Type}

/-! ## Views

The two halves the API observes, derived rather than stored. -/

/-- Every existing location, in depth-first order. -/
def paths (t : Trie V) : List Path := t.entries.map (·.1)

/-- Every location that carries a value, with it, in depth-first order. -/
def vals (t : Trie V) : List (Path × V) :=
  t.entries.filterMap fun e => e.2.map (fun v => (e.1, v))

/-! ## Canonicalisation -/

/-- Deduplicate an association list, keeping the *first* binding for each key.
Left-biased, matching `pathmap`'s `Identity(SELF_IDENT)` value instances. -/
def dedupVals (l : List (Path × V)) : List (Path × V) :=
  l.foldl (fun acc kv => if acc.any (fun x => x.1 == kv.1) then acc else acc ++ [kv]) []

/-- Insert into a key-sorted association list (assumes the key is not present). -/
def insertValSorted (kv : Path × V) : List (Path × V) → List (Path × V)
  | [] => [kv]
  | kv' :: rest =>
      if Path.lt kv.1 kv'.1 then kv :: kv' :: rest
      else kv' :: insertValSorted kv rest

/-- Canonicalise a raw association list: keep the first binding per key, sort by key. -/
def normVals (l : List (Path × V)) : List (Path × V) :=
  (dedupVals l).foldl (fun acc kv => insertValSorted kv acc) []

/-- Build a canonical `Trie` from raw (possibly unsorted, possibly non-closed)
components.  `paths` is completed with every prefix of every raw path and of
every key carrying a value, so callers never have to maintain closure by hand. -/
def mk' (vals : List (Path × V)) (paths : List Path) : Trie V :=
  let vs := normVals vals
  -- Every location that must exist: the root, the ones asked for, the ones
  -- carrying a value, and every prefix of those.  Each then takes whatever
  -- value was bound to it, so a valued location cannot fail to exist.
  let existing := Path.sortDedup
    ((([] : Path) :: (paths ++ vs.map (·.1))).flatMap Path.prefixes)
  { entries := existing.map fun p => (p, vs.lookup p) }

/-- The empty trie: the root exists, nothing else does. -/
def empty : Trie V := { entries := [([], none)] }

instance : Inhabited (Trie V) := ⟨empty⟩

/-! ## Observations

These four functions are the *entire* observable interface of a trie.  Every
specification in `Spec.lean` is phrased in terms of them. -/

/-- `Zipper::val` / `PathMap::get_val_at`: the value at `p`, if any. -/
def valAt (t : Trie V) (p : Path) : Option V := (t.entries.lookup p).join

/-- `Zipper::path_exists`: whether `p` is a location in the trie.  True for
dangling paths (locations with no value and no children). -/
def pathExists (t : Trie V) (p : Path) : Bool := t.paths.contains p

/-- `Zipper::child_mask`: the bytes `b` for which `p ++ [b]` exists. -/
def childMask (t : Trie V) (p : Path) : ByteMask :=
  ByteMask.ofList <| t.paths.filterMap fun q =>
    match Path.stripPrefix p q with
    | some [b] => some b
    | _ => none

/-- `Zipper::child_count`. -/
def childCount (t : Trie V) (p : Path) : Nat := (t.childMask p).length

/-- `ZipperMoving::val_count`: values at and below `p`. -/
def valCount (t : Trie V) (p : Path) : Nat :=
  (t.vals.filter (fun kv => p ≼ kv.1)).length

/-- The existing locations at or below `p`, in depth-first (lexicographic) order. -/
def pathsBelow (t : Trie V) (p : Path) : List Path := t.paths.filter (fun q => p ≼ q)

/-- `TrieNode::node_is_empty` applied to the node *below* `p`: no descendants at all. -/
def belowIsEmpty (t : Trie V) (p : Path) : Bool :=
  t.paths.all (fun q => !(p ≼ q) || q == p)

/-- `PathMap::is_empty`: no root value and an empty root node. -/
def isEmptyMap (t : Trie V) : Bool := t.vals.isEmpty && t.belowIsEmpty []

/-! ## Sub-tries and grafting -/

/-- The subtrie rooted at `p`, **including** the value at `p` as its root value.
This is `make_map` / `take_map` under the default `graft_root_vals` feature, and
also what a zipper rooted at `p` sees.  Yields `empty` when `p` does not exist. -/
def subtrie (t : Trie V) (p : Path) : Trie V :=
  mk' (t.vals.filterMap fun kv => (Path.stripPrefix p kv.1).map (·, kv.2))
      (t.paths.filterMap fun q => Path.stripPrefix p q)

/-- Remove everything at and below `p` (`p` itself stops existing). -/
def removeAt (t : Trie V) (p : Path) : Trie V :=
  mk' (t.vals.filter (fun kv => !(p ≼ kv.1))) (t.paths.filter (fun q => !(p ≼ q)))

/-- Remove everything strictly below `p`; `p` itself, and its value, are untouched.
This is `ZipperWriting::remove_branches` without pruning. -/
def removeBelow (t : Trie V) (p : Path) : Trie V :=
  mk' (t.vals.filter (fun kv => !(p ≼ kv.1) || kv.1 == p))
      (t.paths.filter (fun q => !(p ≼ q) || q == p))

/-- Replace everything strictly below `p` with the strictly-below part of `s`.

The value at `p` is *not* touched (`graft_internal` only ever replaces a node);
`p` is created iff `s` has any non-root content, mirroring the fact that
grafting an empty node neither creates nor destroys the location. -/
def graftBelow (t : Trie V) (p : Path) (s : Trie V) : Trie V :=
  let cleared := t.removeBelow p
  mk' (cleared.vals ++ s.vals.filterMap
        (fun kv => if kv.1 == ([] : Path) then none else some (p ++ kv.1, kv.2)))
      (cleared.paths ++ s.paths.filterMap
        (fun q => if q == ([] : Path) then none else some (p ++ q)))

/-! ## Point updates -/

/-- `ZipperWriting::set_val` / `PathMap::set_val_at`.  Creates `p` if needed.
Returns the replaced value. -/
def setVal (t : Trie V) (p : Path) (v : V) : Option V × Trie V :=
  (t.valAt p, mk' ((p, v) :: t.vals.filter (fun kv => !(kv.1 == p))) t.paths)

/-- `ZipperWriting::remove_val` *without* pruning: the location survives as a
dangling path.  Returns the removed value. -/
def removeVal (t : Trie V) (p : Path) : Option V × Trie V :=
  (t.valAt p, mk' (t.vals.filter (fun kv => !(kv.1 == p))) t.paths)

/-- `ZipperWriting::create_path`: make `p` exist as a dangling path. -/
def addPath (t : Trie V) (p : Path) : Trie V := mk' t.vals (p :: t.paths)

/-! ## Pruning

`prune_path` deletes the dangling chain ending at the focus, stopping at the
first location above it that carries a value, that branches, or that is the
zipper's root.  It is a no-op unless the focus is a *dangling tip*: it exists,
has no value, and has no children. -/

/-- Is `p` a dangling tip — an existing location with neither value nor children? -/
def isDanglingTip (t : Trie V) (p : Path) : Bool :=
  t.pathExists p && (t.valAt p).isNone && t.belowIsEmpty p

/-- The number of bytes `prune_path` would remove at focus `p` for a zipper whose
root sits at depth `rootLen`.  `0` means "nothing to prune". -/
def pruneCount (t : Trie V) (rootLen : Nat) (p : Path) : Nat :=
  if p.length ≤ rootLen then 0
  else if !t.isDanglingTip p then 0
  else
    -- The chain is removed back to the deepest strict ancestor that must be
    -- kept: one carrying a value, one that branches, or the one at the stop
    -- depth.  `properPrefixes` is shortest-first, so the last qualifying entry
    -- is the deepest; the ancestor at `rootLen` always qualifies, so there is
    -- always an answer.
    let stops := (Path.properPrefixes p).filter fun a =>
      a.length ≤ rootLen || (t.valAt a).isSome || t.childCount a ≥ 2
    let a := (stops.getLast?).getD []
    -- `max` is semantically a no-op -- the filter cannot select an ancestor
    -- shallower than `rootLen` once one at `rootLen` qualifies -- but it makes
    -- the "never prunes above the stop depth" bound syntactic, so
    -- `Spec.pruneCount_le` needs no reasoning about which ancestor was picked.
    p.length - (max rootLen a.length)

/-- `ZipperWriting::prune_path`: returns the number of bytes removed and the
pruned trie. -/
def prunePath (t : Trie V) (rootLen : Nat) (p : Path) : Nat × Trie V :=
  let n := t.pruneCount rootLen p
  if n == 0 then (0, t) else (n, t.removeAt (p.take (p.length - n + 1)))

/-! ## Algebraic operations

These lift `pathmap`'s node-level `pjoin` / `pmeet` / `psubtract` / `prestrict`
to whole tries.  The interesting content is *which locations survive*, since
`pathmap` drops any node that ends up empty. -/

variable (ops : ValOps V)

/-- `Option<V>::pjoin` from `src/ring.rs`. -/
def joinVal : Option V → Option V → Option V
  | none, b => b
  | some a, none => some a
  | some a, some b => (ops.pjoin a b).resolve a b

/-- `Option<V>::pmeet`: a value survives only where *both* sides have one. -/
def meetVal : Option V → Option V → Option V
  | some a, some b => (ops.pmeet a b).resolve a b
  | _, _ => none

/-- `Option<V>::psubtract`. -/
def subVal : Option V → Option V → Option V
  | none, _ => none
  | some a, none => some a
  | some a, some b => (ops.psub a b).resolve a b

/-- Join (union).  Every location of either side survives; colliding values are
combined with `ValOps.join`. -/
def join (a b : Trie V) : Trie V :=
  let keys := Path.sortDedup (a.vals.map (·.1) ++ b.vals.map (·.1))
  mk' (keys.filterMap fun k => (joinVal ops (a.valAt k) (b.valAt k)).map (k, ·))
      (a.paths ++ b.paths)

/-- Meet (intersection).  A location survives only if it lies on the way to a
surviving value, so dangling paths never survive a meet. -/
def meet (a b : Trie V) : Trie V :=
  let keys := Path.sortDedup (a.vals.map (·.1))
  mk' (keys.filterMap fun k => (meetVal ops (a.valAt k) (b.valAt k)).map (k, ·)) []

/-- Subtract.

Two rules interact here.  Where `b` has no node at all, `a`'s subtree is kept
verbatim — *including its dangling paths*.  Where `b` does have a node, only
locations leading to a surviving value are kept.  `pathmap` gets this from
`psubtract_dyn` short-circuiting on absent children; the model reproduces it by
splitting on whether the location leaves `b.paths`. -/
def sub (a b : Trie V) : Trie V :=
  let survivingVals : List (Path × V) :=
    a.vals.filterMap fun kv => (subVal ops (some kv.2) (b.valAt kv.1)).map (kv.1, ·)
  let keys := survivingVals.map (·.1)
  let untouched := a.paths.filter fun q =>
    !(q.length == 0) && !b.pathExists q && b.pathExists (q.take (q.length - 1))
  let keptPaths := a.paths.filter fun q =>
    keys.any (fun k => q ≼ k) || untouched.any (fun u => u ≼ q)
  mk' survivingVals keptPaths

/-- Is `q` *validated* by `b` — does some non-empty prefix of `q` carry a value in `b`?

This is the node-level reading of `prestrict`: a node has no root value, so the
empty prefix never validates.  `PathMap::restrict` adds the empty prefix back in
(see `Map.restrict`), which is why the map-level and zipper-level operations
disagree when the source has a root value. -/
def validatedBy (b : Trie V) (q : Path) : Bool :=
  (List.range q.length).any fun i => (b.valAt (q.take (i + 1))).isSome

/-- `prestrict` at node level: keep the locations of `a` validated by `b`.
Once a location is validated, everything below it is kept verbatim. -/
def restrictBelowRoot (a b : Trie V) : Trie V :=
  mk' (a.vals.filter (fun kv => validatedBy b kv.1))
      (a.paths.filter (fun q => validatedBy b q))

/-- Structural (hence observational) equality of tries.  Used to decide
`AlgebraicStatus::Identity` — `pathmap` reports `Identity(SELF_IDENT)` exactly
when the operation's output equals `self`. -/
def beqT (ops : ValOps V) (a b : Trie V) : Bool :=
  a.entries.length == b.entries.length &&
  (a.entries.zip b.entries).all fun xy =>
    xy.1.1 == xy.2.1 &&
      (match xy.1.2, xy.2.2 with
       | none, none => true
       | some x, some y => ops.beq x y
       | _, _ => false)

/-! ## Path surgery -/

/-- `ZipperWriting::insert_prefix`: put `k` in front of every path below the root. -/
def insertPrefixBelow (t : Trie V) (k : Path) : Trie V :=
  mk' (t.vals.filterMap fun kv => if kv.1 == ([] : Path) then none else some (k ++ kv.1, kv.2))
      (t.paths.filterMap fun q => if q == ([] : Path) then none else some (k ++ q))

/-- The existing locations exactly `k` bytes below the root, in depth-first order. -/
def kPaths (t : Trie V) (k : Nat) : List Path := t.paths.filter (fun q => q.length == k)

/-- `drop_head` / `ZipperWriting::join_k_path_into` at node level: strip the first
`k` bytes from every path and join the results.

Values sitting at depth *exactly* `k` are **discarded** — the joined node has
nowhere to put a root value.  (`meet_k_path_into` keeps them, because it routes
through `take_map`/`graft_map`, which do carry root values.  The asymmetry is
real; see `Spec.lean`.) -/
def dropHead (t : Trie V) (k : Nat) : Trie V :=
  if k == 0 then t
  else (t.kPaths k).foldl (fun acc q => join ops acc ((t.subtrie q).removeVal []).2) empty

end Trie
end PathMapModel
