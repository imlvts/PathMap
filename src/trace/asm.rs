//! Runtime for the `trace_asm` backend: list trace sites, patch them on and off, and route
//! enabled sites to a handler
//!
//! ```ignore
//! use pathmap::trace::{Event, asm};
//!
//! fn handler(event: Event, args: &[u64]) {
//!     eprintln!("{} {:?}", event.name(), args);
//! }
//!
//! asm::set_handler(Some(handler));
//! asm::enable(|site| site.event() == Event::descend_to).unwrap();
//! // ... run code that uses zippers ...
//! asm::disable_all().unwrap();
//! ```
//!
//! ## How patching works
//!
//! Each site starts as a `nop5` inside one aligned 8-byte word.  Enabling a site rewrites the
//! word with a single atomic store: the first five bytes become `jmp rel32` to the site's
//! out-of-line block, and the other three are written back unchanged.  Disabling stores the
//! `nop5` again.  The text page is made writable with `mprotect` around the store.  After a
//! batch, `membarrier(PRIVATE_EXPEDITED_SYNC_CORE)` serializes every thread of the process, so
//! no thread keeps running the old bytes.
//!
//! Requirements and caveats:
//! - Text pages are mapped `rwx` for the duration of each store.  This fails under strict W^X
//!   policies (SELinux `execmod` denials, PaX), and [enable] returns the `mprotect` error.
//! - On kernels without `membarrier` sync-core support (before Linux 4.16), the sync step is
//!   skipped.  Other threads then pick up the change some time later.
//! - Patching is serialized by a global lock, but nothing stops two different runtimes (for
//!   example two copies of pathmap in one binary) from patching the same page at once.

use super::Event;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::ffi::CStr;
use std::io;
use std::sync::Mutex;

/// The record each trace site writes into the `pathmap_trace_sites` section
#[repr(C)]
struct SiteRecord {
    /// Address of the `nop5`
    addr: usize,
    /// Address of the out-of-line block
    target: usize,
    /// [Event] discriminant, or [ANCHOR_EVENT] for the anchor record
    event: u64,
    /// NUL-terminated `file:line`
    location: *const core::ffi::c_char,
}

/// Marks the anchor record, which is not a site
const ANCHOR_EVENT: u64 = u64::MAX;

// The anchor guarantees the section exists, so the linker defines the `__start_`/`__stop_`
// symbols even when no trace site is linked.  The flags must match the ones in `trace_point!`
core::arch::global_asm!(
    ".pushsection pathmap_trace_sites, \"awR\", @progbits",
    ".balign 8",
    ".globl pathmap_trace_sites_anchor",
    ".hidden pathmap_trace_sites_anchor",
    "pathmap_trace_sites_anchor:",
    ".quad 0, 0, {anchor}, 0",
    ".popsection",
    anchor = const ANCHOR_EVENT,
);

unsafe extern "C" {
    static pathmap_trace_sites_anchor: SiteRecord;
    static __start_pathmap_trace_sites: SiteRecord;
    static __stop_pathmap_trace_sites: SiteRecord;
}

fn records() -> &'static [SiteRecord] {
    unsafe {
        // Referencing the anchor keeps its object file, and so the section, in the link
        core::hint::black_box(&raw const pathmap_trace_sites_anchor);
        let start = &raw const __start_pathmap_trace_sites;
        let stop = &raw const __stop_pathmap_trace_sites;
        let len = (stop as usize - start as usize) / core::mem::size_of::<SiteRecord>();
        core::slice::from_raw_parts(start, len)
    }
}

/// One trace site in the binary
///
/// The compiler copies a site whenever it inlines or monomorphizes the code that holds it, so
/// one `trace_point!` in the source can be many sites.
#[derive(Clone, Copy)]
pub struct Site {
    record: &'static SiteRecord,
    event: Event,
}

impl Site {
    /// The event this site emits
    pub fn event(&self) -> Event { self.event }

    /// The `file:line` of the `trace_point!` that produced the site
    pub fn location(&self) -> &'static str {
        unsafe { CStr::from_ptr(self.record.location) }.to_str().unwrap_or("?")
    }

    /// The address of the site's patchable instruction
    pub fn address(&self) -> usize { self.record.addr }

    /// Returns `true` if the site is patched to call the handler
    pub fn is_enabled(&self) -> bool {
        unsafe { core::ptr::read_volatile(self.record.addr as *const u8) == JMP_REL32 }
    }
}

impl core::fmt::Debug for Site {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Site")
            .field("event", &self.event)
            .field("location", &self.location())
            .field("address", &format_args!("{:#x}", self.address()))
            .field("enabled", &self.is_enabled())
            .finish()
    }
}

/// Returns every trace site linked into the binary
pub fn sites() -> impl Iterator<Item = Site> {
    records().iter().filter_map(|record| {
        // A `?` here trips `bindings_with_variant_name`, because its desugaring binds `val`
        Event::from_id(record.event).map(|event| Site { record, event })
    })
}

// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---
// Handler
// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---

/// Called by every enabled site with the event and its arguments (see [Event::arg_names])
///
/// The handler runs inside zipper methods, so it must not use the zipper that emitted the
/// event, and anything it does with other traced zippers produces more events.
pub type Handler = fn(Event, &[u64]);

static HANDLER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Sets the handler called by enabled sites, or removes it with `None`
///
/// Enabled sites with no handler do nothing.
pub fn set_handler(handler: Option<Handler>) {
    let ptr = match handler {
        Some(handler) => handler as *mut (),
        None => core::ptr::null_mut(),
    };
    HANDLER.store(ptr, Ordering::Release);
}

/// The out-of-line part of every site
#[doc(hidden)]
#[cold]
#[inline(never)]
pub fn fire(event: Event, args: &[u64]) {
    let ptr = HANDLER.load(Ordering::Acquire);
    if !ptr.is_null() {
        let handler: Handler = unsafe { core::mem::transmute::<*mut (), Handler>(ptr) };
        handler(event, args);
    }
}

// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---
// Patching
// ***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---***---

const NOP5: [u8; 5] = [0x0f, 0x1f, 0x44, 0x00, 0x00];
const JMP_REL32: u8 = 0xe9;

static PATCH_LOCK: Mutex<()> = Mutex::new(());

/// Enables every site for which `filter` returns `true`, and returns how many sites changed
///
/// Sites the filter rejects are left as they are.
pub fn enable(filter: impl FnMut(&Site) -> bool) -> io::Result<usize> {
    patch(filter, true)
}

/// Disables every site for which `filter` returns `true`, and returns how many sites changed
pub fn disable(filter: impl FnMut(&Site) -> bool) -> io::Result<usize> {
    patch(filter, false)
}

/// Enables every site
pub fn enable_all() -> io::Result<usize> { enable(|_| true) }

/// Disables every site
pub fn disable_all() -> io::Result<usize> { disable(|_| true) }

fn patch(mut filter: impl FnMut(&Site) -> bool, enable: bool) -> io::Result<usize> {
    let _guard = PATCH_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let page_size = page_size();
    let mut changed = 0;
    let mut result = Ok(());
    for site in sites() {
        if site.is_enabled() == enable || !filter(&site) {
            continue;
        }
        let bytes = if enable { jmp_bytes(site.record)? } else { NOP5 };
        if let Err(err) = unsafe { write_site(site.record.addr, bytes, page_size) } {
            result = Err(err);
            break;
        }
        changed += 1;
    }
    if changed > 0 {
        sync_cores();
    }
    result.map(|()| changed)
}

fn jmp_bytes(record: &SiteRecord) -> io::Result<[u8; 5]> {
    let rel = (record.target as i64).wrapping_sub(record.addr as i64 + 5);
    let rel = i32::try_from(rel).map_err(|_| io::Error::other("trace site target out of jmp rel32 range"))?;
    let [a, b, c, d] = rel.to_le_bytes();
    Ok([JMP_REL32, a, b, c, d])
}

/// Replaces the first five bytes of the aligned word at `addr`
///
/// SAFETY: `addr` must be a trace site from the site table
unsafe fn write_site(addr: usize, bytes: [u8; 5], page_size: usize) -> io::Result<()> {
    debug_assert_eq!(addr % 8, 0);
    let page = addr & !(page_size - 1);
    unsafe {
        mprotect(page, page_size, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC)?;
        let word = AtomicU64::from_ptr(addr as *mut u64);
        let mut new = word.load(Ordering::Relaxed).to_le_bytes();
        new[..5].copy_from_slice(&bytes);
        word.store(u64::from_le_bytes(new), Ordering::SeqCst);
        mprotect(page, page_size, libc::PROT_READ | libc::PROT_EXEC)
    }
}

unsafe fn mprotect(page: usize, len: usize, prot: libc::c_int) -> io::Result<()> {
    if unsafe { libc::mprotect(page as *mut libc::c_void, len, prot) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn page_size() -> usize {
    match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
        size if size > 0 => size as usize,
        _ => 4096,
    }
}

const MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE: libc::c_int = 1 << 5;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE: libc::c_int = 1 << 6;

/// Makes every thread of the process execute a serializing instruction before it runs more
/// user code, so none of them keeps executing the bytes from before a patch
fn sync_cores() {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let registered = *REGISTERED.get_or_init(|| unsafe {
        libc::syscall(libc::SYS_membarrier, MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE, 0, 0) == 0
    });
    if registered {
        unsafe { libc::syscall(libc::SYS_membarrier, MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE, 0, 0) };
    }
}
