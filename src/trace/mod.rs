//! Zipper tracing
//!
//! Every zipper carries a [ZipperUid], and every zipper trait method it implements has a trace
//! point at entry.  A trace point is a `trace_point!` invocation.  What it compiles to is picked
//! by a cargo feature:
//!
//! | feature      | trace point compiles to                                                         |
//! |--------------|---------------------------------------------------------------------------------|
//! | (none)       | nothing.  Arguments are not evaluated                                           |
//! | `trace_usdt` | a SystemTap SDT probe (`nop` + `.note.stapsdt`), provider `pathmap`.  Attach with bpftrace, perf, bcc, SystemTap.  Linux only |
//! | `trace_asm`  | a patchable `nop5` site.  Disabled sites cost one nop; `asm::enable` patches selected sites into a `jmp` to an out-of-line block that calls the handler set with `asm::set_handler` |
//!
//! `trace_usdt` and `trace_asm` are mutually exclusive.  Both imply `trace_uid`, which makes
//! [ZipperUid] a real id.  Without `trace_uid`, [ZipperUid] is zero-sized and [ZipperUid::get]
//! returns 0.
//!
//! ## Event arguments
//!
//! Every argument is a `u64`.  The first argument of every event is the uid of the zipper the
//! event happened on.  The remaining arguments are listed by [Event::arg_names]:
//!
//! - a byte path is passed as two arguments: a pointer to the bytes and the length
//! - a source zipper is passed as its uid (see [crate::zipper::Zipper::trace_uid]), 0 if untraced
//! - a value `V` or a [ByteMask](crate::utils::ByteMask) is passed as a pointer to it
//!
//! Pointers are only valid while the probe or handler runs.
//!
//! Events fire on entry, before the operation runs.  A zipper method that calls other methods
//! of the same zipper produces nested events with the same uid.
//!
//! ## Example: bpftrace with `trace_usdt`
//!
//! ```sh
//! bpftrace -e 'usdt:./target/release/app:pathmap:descend_to { printf("%d %r\n", arg0, buf(arg1, arg2)); }'
//! ```

#[cfg(all(feature = "trace_usdt", feature = "trace_asm"))]
compile_error!("features `trace_usdt` and `trace_asm` are mutually exclusive");

#[cfg(all(feature = "trace_asm", not(all(target_arch = "x86_64", target_os = "linux"))))]
compile_error!("feature `trace_asm` is only implemented for x86_64 Linux");

#[cfg(all(feature = "trace_usdt", not(target_os = "linux")))]
compile_error!("feature `trace_usdt` emits SystemTap SDT notes, which are only implemented for Linux");

#[cfg(all(feature = "trace_asm", target_arch = "x86_64", target_os = "linux"))]
pub mod asm;

// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---
// The trace point macro
// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---

/// Declares a trace point: `trace_point!(event, uid, args...)`
///
/// `event` must name an [Event] variant.  Each argument is an expression implementing
/// [TraceArg].  See the [module docs](self) for what this compiles to.
#[cfg(not(any(feature = "trace_usdt", feature = "trace_asm")))]
macro_rules! trace_point {
    ($event:ident $(, $arg:expr)* $(,)?) => {{
        // Keeps event names checked.  The arguments are dropped unexpanded, so not even a debug
        // build carries code for them
        const _: $crate::trace::Event = $crate::trace::Event::$event;
    }};
}

/// A SystemTap SDT probe: a `nop` plus a `.note.stapsdt` record giving the provider, the probe
/// name, the `nop`'s address and where each argument lives (`8@%reg`).  This is the format
/// `<sys/sdt.h>` and the `probe` crate produce, with one difference: the note is linked to the
/// code that holds the probe (`SHF_LINK_ORDER`), so the linker drops the note when it drops the
/// function.  Without that, `--gc-sections` (on by default for Rust executables) leaves notes
/// pointing at discarded code, and libbpf then refuses to attach the whole probe.
///
/// The link needs a symbol unique to each copy of the asm block, and inline asm can't make one,
/// so the assembler does: a counter symbol is bumped per probe and expanded into the label name
/// with `.altmacro`.
#[cfg(all(feature = "trace_usdt", not(feature = "trace_asm")))]
macro_rules! trace_point {
    ($event:ident $(, $arg:expr)* $(,)?) => {{
        const _: $crate::trace::Event = $crate::trace::Event::$event;
        #[allow(unused_unsafe)]
        unsafe {
            ::core::arch::asm!(
                ".ifndef .Lpathmap_sdt_n",
                ".set .Lpathmap_sdt_n, 0",
                ".endif",
                ".set .Lpathmap_sdt_n, .Lpathmap_sdt_n + 1",
                ".altmacro",
                ".macro pathmap_sdt_open n",
                ".Lpathmap_sdt_\\n: nop",
                ".pushsection .note.stapsdt, \"o\", \"note\", .Lpathmap_sdt_\\n",
                ".balign 4",
                ".4byte 992f-991f, 994f-993f, 3",
                "991: .asciz \"stapsdt\"",
                "992: .balign 4",
                "993: .8byte .Lpathmap_sdt_\\n",
                ".endm",
                "pathmap_sdt_open %(.Lpathmap_sdt_n)",
                ".purgem pathmap_sdt_open",
                ".noaltmacro",
                ".8byte _.stapsdt.base",
                ".8byte 0", // no semaphore
                ".asciz \"pathmap\"",
                concat!(".asciz \"", stringify!($event), "\""),
                concat!(".asciz \"", $crate::trace::sdt_arg_specs!($($arg),*), "\""),
                "994: .balign 4",
                ".popsection",
                // The base address tools use to adjust for prelinking, one per binary
                ".ifndef _.stapsdt.base",
                ".pushsection .stapsdt.base, \"aGR\", \"progbits\", .stapsdt.base, comdat",
                ".weak _.stapsdt.base",
                ".hidden _.stapsdt.base",
                "_.stapsdt.base: .space 1",
                ".size _.stapsdt.base, 1",
                ".popsection",
                ".endif",
                $( in(reg) $crate::trace::TraceArg::trace_arg(&$arg), )*
                options(readonly, nostack, preserves_flags),
                #[cfg(target_arch = "x86_64")]
                options(att_syntax),
            );
        }
    }};
}

/// The SDT argument string for `trace_point!`: one `8@{}` (unsigned 64-bit, in the register the
/// operand gets) per argument
#[cfg(all(feature = "trace_usdt", not(feature = "trace_asm")))]
macro_rules! sdt_arg_specs {
    ($a:expr) => { "8@{}" };
    ($a:expr, $($rest:expr),+) => { concat!("8@{} ", $crate::trace::sdt_arg_specs!($($rest),+)) };
}

#[cfg(all(feature = "trace_usdt", not(feature = "trace_asm")))]
pub(crate) use sdt_arg_specs;

/// Each site is a `nop5` kept inside one aligned 8-byte word, so the patcher can swap it for a
/// `jmp rel32` with a single atomic store.  A 32-byte record `(site, target, event, location)`
/// goes in the `pathmap_trace_sites` section, which the runtime walks with the linker's
/// `__start_`/`__stop_` symbols.  The `label` block holds the argument computation and the
/// handler call; it only runs once the site is patched.
#[cfg(all(feature = "trace_asm", not(feature = "trace_usdt"), target_arch = "x86_64", target_os = "linux"))]
macro_rules! trace_point {
    ($event:ident $(, $arg:expr)* $(,)?) => {{
        #[allow(unused_unsafe)]
        unsafe {
            ::core::arch::asm!(
                ".balign 8",
                "2: .byte 0x0f, 0x1f, 0x44, 0x00, 0x00",
                ".pushsection pathmap_trace_sites, \"awR\", @progbits",
                ".balign 8",
                ".quad 2b, {slow}, {event}, 3f",
                ".popsection",
                ".pushsection .rodata.pathmap_trace_locations, \"a\", @progbits",
                concat!("3: .asciz \"", file!(), ":", line!(), "\""),
                ".popsection",
                event = const $crate::trace::Event::$event as u64,
                slow = label {
                    $crate::trace::asm::fire($crate::trace::Event::$event, &[$($crate::trace::TraceArg::trace_arg(&$arg)),*]);
                },
                options(nostack),
            );
        }
    }};
}

pub(crate) use trace_point;

/// Creates the [ZipperUid] for a new zipper: `new_uid!(ReadZipperUntracked)`, naming a
/// [ZipperKind] variant
///
/// Without `trace_uid` this is a plain zero-sized value rather than a call, so the zipper's
/// constructor compiles exactly as if the field weren't there, even in debug builds.
#[cfg(feature = "trace_uid")]
macro_rules! new_uid {
    ($kind:ident) => { $crate::trace::ZipperUid::new($crate::trace::ZipperKind::$kind) };
}

#[cfg(not(feature = "trace_uid"))]
macro_rules! new_uid {
    ($kind:ident) => {{
        const _: $crate::trace::ZipperKind = $crate::trace::ZipperKind::$kind;
        $crate::trace::ZipperUid::OFF
    }};
}

pub(crate) use new_uid;

// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---
// Arguments
// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---

/// Conversion of a trace point argument to the `u64` passed to tracers
pub trait TraceArg {
    /// Returns the value passed to the tracer
    fn trace_arg(&self) -> u64;
}

macro_rules! impl_trace_arg_as {
    ($($t:ty),*) => { $( impl TraceArg for $t { #[inline(always)] fn trace_arg(&self) -> u64 { *self as u64 } } )* };
}
impl_trace_arg_as!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, bool);

impl<T: TraceArg + ?Sized> TraceArg for &T {
    #[inline(always)]
    fn trace_arg(&self) -> u64 { (**self).trace_arg() }
}

impl<T: TraceArg + ?Sized> TraceArg for &mut T {
    #[inline(always)]
    fn trace_arg(&self) -> u64 { (**self).trace_arg() }
}

impl<T: ?Sized> TraceArg for *const T {
    #[inline(always)]
    fn trace_arg(&self) -> u64 { self.cast::<u8>() as usize as u64 }
}

impl<T: ?Sized> TraceArg for *mut T {
    #[inline(always)]
    fn trace_arg(&self) -> u64 { self.cast::<u8>() as usize as u64 }
}

impl TraceArg for ZipperKind {
    #[inline(always)]
    fn trace_arg(&self) -> u64 { *self as u64 }
}

impl TraceArg for ZipperUid {
    #[inline(always)]
    fn trace_arg(&self) -> u64 { self.get() }
}

// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---
// Events
// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---

macro_rules! define_events {
    ($( $(#[$m:meta])* $name:ident ( $($arg:ident),* ) ),* $(,)?) => {
        /// Every event a trace point can emit.  Variant names are the USDT probe names
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u16)]
        pub enum Event { $( $(#[$m])* $name ),* }

        impl Event {
            /// All events, indexed by their discriminant
            pub const ALL: &'static [Event] = &[$(Event::$name),*];

            /// The event's name, which is also its USDT probe name
            pub const fn name(self) -> &'static str {
                match self { $(Event::$name => stringify!($name)),* }
            }

            /// Names of the event's arguments, in order.  The first is always `uid`
            pub const fn arg_names(self) -> &'static [&'static str] {
                match self { $(Event::$name => &["uid" $(, stringify!($arg))*]),* }
            }

            /// Returns the event with the discriminant `id`
            pub fn from_id(id: u64) -> Option<Self> {
                Self::ALL.get(usize::try_from(id).ok()?).copied()
            }
        }
    };
}

define_events! {
    // Lifecycle
    /// A zipper was created.  `kind` is a [ZipperKind]
    zipper_new(kind),
    /// A zipper was cloned from `parent`.  It has the same [ZipperKind] as `parent`, and gets no
    /// `zipper_new`.  Every zipper gets exactly one of `zipper_new` or `zipper_clone`
    zipper_clone(parent),
    /// A zipper was derived from `parent`, e.g. with `fork_read_zipper` or `into_read_zipper`
    zipper_derive(parent),
    /// A zipper was dropped
    zipper_drop(),

    // Zipper
    path_exists(),
    is_val(),
    child_count(),
    child_mask(),
    // ZipperValues, ZipperValuesAt
    val(),
    val_at(path_ptr, path_len),
    // ZipperForking
    fork_read_zipper(),
    // ZipperSubtries, ZipperInfallibleSubtries, ZipperReadOnlySubtries
    try_make_map(),
    trie_ref(),
    make_map(),
    get_trie_ref(),
    get_focus(),
    get_focus_at(path_ptr, path_len),
    try_borrow_focus(),
    trie_ref_at_path(path_ptr, path_len),
    // ZipperMoving
    depth(),
    at_root(),
    focus_byte(),
    reset(),
    val_count(),
    descend_to(path_ptr, path_len),
    descend_to_check(path_ptr, path_len),
    descend_to_existing(path_ptr, path_len),
    descend_to_val(path_ptr, path_len),
    descend_to_byte(byte),
    descend_to_existing_byte(byte),
    descend_indexed_byte(idx),
    descend_indexed_branch(idx),
    descend_first_byte(),
    descend_last_byte(),
    descend_until(),
    descend_until_observed(),
    descend_until_max_bytes(max_bytes),
    descend_until_max_bytes_observed(max_bytes),
    ascend(steps),
    ascend_byte(),
    ascend_until(),
    ascend_until_branch(),
    to_next_sibling_byte(),
    to_prev_sibling_byte(),
    to_next_step(),
    to_next_step_observed(),
    to_prev_step(),
    to_prev_step_observed(),
    // ZipperPath, ZipperAbsolutePath
    path(),
    move_to_path(path_ptr, path_len),
    origin_path(),
    root_prefix_path(),
    // ZipperIteration
    to_next_val(),
    to_next_val_observed(),
    to_prev_val(),
    to_prev_val_observed(),
    descend_last_val(),
    descend_last_val_observed(),
    descend_last_path(),
    descend_last_path_observed(),
    descend_first_k_path(k),
    descend_first_k_path_observed(k),
    to_next_k_path(k),
    to_next_k_path_observed(k),
    // ZipperReadOnlyValues, ZipperReadOnlyConditionalValues, ZipperReadOnlyIteration
    get_val(),
    get_val_at(path_ptr, path_len),
    get_val_with_witness(),
    to_next_get_val(),
    to_next_get_val_observed(),
    to_next_get_val_with_witness(),
    to_next_get_val_with_witness_observed(),
    // ZipperConcrete
    shared_node_id(),
    is_shared(),
    // ZipperWriting
    get_val_mut(),
    get_val_or_set_mut(default_ptr),
    get_val_or_set_mut_with(),
    set_val(val_ptr),
    set_val_at(path_ptr, path_len, val_ptr),
    remove_val(prune),
    zipper_head(),
    graft(src),
    graft_src_at(src, path_ptr, path_len),
    graft_map(),
    graft_child_maps(mask_ptr, remove_unset),
    graft_masked_branches(src, mask_ptr, remove_unset),
    join_into(src),
    join_map_into(),
    join_into_take(src, prune),
    join_k_path_into(byte_cnt, prune),
    meet_k_path_into(byte_cnt, prune),
    insert_prefix(path_ptr, path_len),
    remove_prefix(n),
    meet_into(src, prune),
    meet_2(src_a, src_b),
    subtract_into(src, prune),
    restrict(src),
    restricting(src),
    take_map(prune),
    remove_branches(prune),
    remove_subtrie(prune),
    remove_unmasked_branches(mask_ptr, prune),
    create_path(),
    prune_path(),
    prune_ascend(),
}

// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---
// Zipper identity
// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---

/// The zipper type, passed with [Event::zipper_new]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u64)]
#[non_exhaustive]
pub enum ZipperKind {
    ReadZipperTracked = 1,
    ReadZipperUntracked = 2,
    ReadZipperOwned = 3,
    WriteZipperTracked = 4,
    WriteZipperUntracked = 5,
    WriteZipperOwned = 6,
    ProductZipper = 7,
    ProductZipperG = 8,
    DependentProductZipperG = 9,
    PrefixZipper = 10,
    OverlayZipper = 11,
    SubtractZipper = 12,
    EmptyZipper = 13,
    ACTZipper = 14,
    DiffZipper = 15,
    FullZipper = 16,
    NullZipper = 17,
}

/// A process-unique zipper id
///
/// With the `trace_uid` feature, creating a `ZipperUid` takes a fresh id and emits
/// [Event::zipper_new], cloning takes a fresh id and emits [Event::zipper_clone], and dropping
/// emits [Event::zipper_drop].  Without the feature it is zero-sized and does nothing.
///
/// Ids start at 1, so 0 never names a zipper.
#[repr(transparent)]
pub struct ZipperUid(#[cfg(feature = "trace_uid")] u64);

// With tracing off, zippers must not grow
#[cfg(not(feature = "trace_uid"))]
const _: () = assert!(core::mem::size_of::<ZipperUid>() == 0 && core::mem::align_of::<ZipperUid>() == 1);

/// A copy of a [ZipperUid]'s id, for naming a zipper in an event after the zipper is consumed.
/// Zero-sized without the `trace_uid` feature
#[derive(Clone, Copy)]
pub(crate) struct UidSnapshot(#[cfg(feature = "trace_uid")] u64);

impl TraceArg for UidSnapshot {
    #[inline(always)]
    fn trace_arg(&self) -> u64 {
        #[cfg(feature = "trace_uid")]
        { self.0 }
        #[cfg(not(feature = "trace_uid"))]
        { 0 }
    }
}

#[cfg(feature = "trace_uid")]
mod uid_alloc {
    use core::cell::Cell;
    use core::sync::atomic::{AtomicU64, Ordering};

    /// Ids are handed to threads in blocks, so creating a zipper doesn't contend on one counter
    const BLOCK: u64 = 1024;
    static NEXT_BLOCK: AtomicU64 = AtomicU64::new(1);

    std::thread_local! {
        /// `(next, end)` of this thread's block
        static LOCAL: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    }

    #[inline]
    pub(super) fn next_uid() -> u64 {
        LOCAL.with(|local| {
            let (next, end) = local.get();
            if next != end {
                local.set((next + 1, end));
                next
            } else {
                let start = NEXT_BLOCK.fetch_add(BLOCK, Ordering::Relaxed);
                local.set((start + 1, start + BLOCK));
                start
            }
        })
    }
}

impl ZipperUid {
    /// The value of every `ZipperUid` without the `trace_uid` feature
    #[cfg(not(feature = "trace_uid"))]
    pub(crate) const OFF: Self = Self();

    /// Takes a fresh id and emits [Event::zipper_new].  Use [new_uid!], which compiles to
    /// nothing without the `trace_uid` feature
    #[cfg(feature = "trace_uid")]
    #[inline]
    #[allow(unused_variables)] // `kind` is only read by a trace point
    pub(crate) fn new(kind: ZipperKind) -> Self {
        let uid = Self(uid_alloc::next_uid());
        trace_point!(zipper_new, uid, kind);
        uid
    }

    /// Copies the id, see [UidSnapshot]
    #[inline(always)]
    pub(crate) fn snapshot(&self) -> UidSnapshot {
        #[cfg(feature = "trace_uid")]
        { UidSnapshot(self.0) }
        #[cfg(not(feature = "trace_uid"))]
        { UidSnapshot() }
    }

    /// Returns the id, or 0 without the `trace_uid` feature
    #[inline(always)]
    pub fn get(&self) -> u64 {
        #[cfg(feature = "trace_uid")]
        { self.0 }
        #[cfg(not(feature = "trace_uid"))]
        { 0 }
    }
}

impl Clone for ZipperUid {
    /// A clone is a different zipper, so it takes a fresh id
    #[cfg_attr(feature = "trace_uid", inline)]
    #[cfg_attr(not(feature = "trace_uid"), inline(always))]
    fn clone(&self) -> Self {
        #[cfg(feature = "trace_uid")]
        {
            let uid = Self(uid_alloc::next_uid());
            trace_point!(zipper_clone, uid, self);
            uid
        }
        #[cfg(not(feature = "trace_uid"))]
        { Self() }
    }
}

#[cfg(feature = "trace_uid")]
impl Drop for ZipperUid {
    #[inline]
    fn drop(&mut self) {
        trace_point!(zipper_drop, self);
    }
}

impl core::fmt::Debug for ZipperUid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ZipperUid({})", self.get())
    }
}

#[cfg(all(test, feature = "trace_uid"))]
mod tests {
    use super::*;

    #[test]
    fn uids_are_unique_and_clones_differ() {
        let a = new_uid!(EmptyZipper);
        let b = a.clone();
        let c = new_uid!(EmptyZipper);
        assert!(a.get() != 0 && b.get() != 0 && c.get() != 0);
        assert!(a.get() != b.get() && b.get() != c.get() && a.get() != c.get());
        let other = std::thread::spawn(|| new_uid!(EmptyZipper).get()).join().unwrap();
        assert!(other != a.get() && other != b.get() && other != c.get());
    }

    #[test]
    fn events_round_trip() {
        for (i, e) in Event::ALL.iter().enumerate() {
            assert_eq!(*e as usize, i);
            assert_eq!(Event::from_id(i as u64), Some(*e));
            assert_eq!(e.arg_names()[0], "uid");
        }
    }
}
