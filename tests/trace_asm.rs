//! Tests the `trace_asm` backend: sites start disabled, enabling patches them to call the
//! handler, and disabling restores the nops
#![cfg(all(feature = "trace_asm", target_arch = "x86_64", target_os = "linux"))]

use pathmap::PathMap;
use pathmap::trace::{asm, Event, ZipperKind};
use pathmap::zipper::*;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
enum Rec {
    New { uid: u64, kind: u64 },
    Clone { uid: u64, parent: u64 },
    Drop { uid: u64 },
    DescendTo { uid: u64, path: Vec<u8> },
    DescendToByte { uid: u64, byte: u8 },
    SetVal { uid: u64, val: u64 },
    Graft { uid: u64, src: u64 },
}

static LOG: Mutex<Vec<Rec>> = Mutex::new(Vec::new());

fn handler(event: Event, args: &[u64]) {
    assert_eq!(args.len(), event.arg_names().len(), "{event:?}");
    let rec = match event {
        Event::zipper_new => Rec::New { uid: args[0], kind: args[1] },
        Event::zipper_clone => Rec::Clone { uid: args[0], parent: args[1] },
        Event::zipper_drop => Rec::Drop { uid: args[0] },
        Event::descend_to => {
            // The pointer is only valid while the handler runs, so copy the bytes now
            let path = unsafe { std::slice::from_raw_parts(args[1] as *const u8, args[2] as usize) };
            Rec::DescendTo { uid: args[0], path: path.to_vec() }
        }
        Event::descend_to_byte => Rec::DescendToByte { uid: args[0], byte: args[1] as u8 },
        Event::set_val => Rec::SetVal { uid: args[0], val: unsafe { *(args[1] as *const u64) } },
        Event::graft => Rec::Graft { uid: args[0], src: args[1] },
        _ => return,
    };
    LOG.lock().unwrap().push(rec);
}

fn take_log() -> Vec<Rec> {
    core::mem::take(&mut *LOG.lock().unwrap())
}

const TRACED: &[Event] = &[
    Event::zipper_new, Event::zipper_clone, Event::zipper_drop, Event::descend_to,
    Event::descend_to_byte, Event::set_val, Event::graft,
];

#[test]
fn trace_asm_sites() {
    let sites: Vec<_> = asm::sites().collect();
    assert!(sites.len() > 100, "expected many sites, found {}", sites.len());
    assert!(sites.iter().all(|site| !site.is_enabled()), "sites must start disabled");
    assert!(sites.iter().all(|site| site.address() % 8 == 0));
    assert!(sites.iter().any(|site| site.event() == Event::descend_to && site.location().contains("src/")));

    // Disabled sites never reach the handler
    asm::set_handler(Some(handler));
    let mut map = PathMap::<u64>::new();
    map.write_zipper().set_val(1);
    assert!(take_log().is_empty());

    let enabled = asm::enable(|site| TRACED.contains(&site.event())).unwrap();
    assert!(enabled > 0);
    assert!(asm::sites().filter(|site| TRACED.contains(&site.event())).all(|site| site.is_enabled()));
    assert!(asm::sites().any(|site| !site.is_enabled()), "unselected sites stay disabled");
    // Enabling again changes nothing
    assert_eq!(asm::enable(|site| TRACED.contains(&site.event())).unwrap(), 0);

    let (wz_uid, rz_uid, clone_uid) = {
        let mut wz = map.write_zipper_at_path(b"ab");
        wz.descend_to_byte(b'c');
        wz.set_val(42);
        let wz_uid = wz.trace_uid();
        drop(wz);

        let mut rz = map.read_zipper();
        rz.descend_to(b"abc");
        assert_eq!(rz.val(), Some(&42));
        let rz_uid = rz.trace_uid();
        let rz2 = rz.clone();
        let clone_uid = rz2.trace_uid();

        let mut dst = PathMap::<u64>::new();
        let mut dst_wz = dst.write_zipper();
        dst_wz.graft(&rz2);
        let dst_uid = dst_wz.trace_uid();
        drop(dst_wz);
        assert_eq!(dst.read_zipper().val(), Some(&42));
        let log = take_log();
        assert!(log.contains(&Rec::Graft { uid: dst_uid, src: clone_uid }), "{log:#?}");
        put_back(log);
        (wz_uid, rz_uid, clone_uid)
    };
    let log = take_log();

    assert!(wz_uid != 0 && rz_uid != 0 && clone_uid != 0);
    assert!(rz_uid != clone_uid, "a clone gets a new uid");
    let expect = [
        Rec::New { uid: wz_uid, kind: ZipperKind::WriteZipperUntracked as u64 },
        Rec::DescendToByte { uid: wz_uid, byte: b'c' },
        Rec::SetVal { uid: wz_uid, val: 42 },
        Rec::Drop { uid: wz_uid },
        Rec::New { uid: rz_uid, kind: ZipperKind::ReadZipperUntracked as u64 },
        Rec::DescendTo { uid: rz_uid, path: b"abc".to_vec() },
        Rec::Clone { uid: clone_uid, parent: rz_uid },
        Rec::Drop { uid: clone_uid },
        Rec::Drop { uid: rz_uid },
    ];
    let mut pos = 0;
    for rec in &log {
        if pos < expect.len() && *rec == expect[pos] {
            pos += 1;
        }
    }
    assert_eq!(pos, expect.len(), "missing {:?} in order in {log:#?}", expect.get(pos));
    // A clone has no zipper_new of its own
    assert!(!log.iter().any(|rec| matches!(rec, Rec::New { uid, .. } if *uid == clone_uid)));

    // Disabling restores the nops
    let disabled = asm::disable_all().unwrap();
    assert_eq!(disabled, enabled);
    assert!(asm::sites().all(|site| !site.is_enabled()));
    map.write_zipper().set_val(2);
    assert!(take_log().is_empty());

    // Sites keep working when other threads run through them while they are patched
    asm::enable(|site| site.event() == Event::descend_to_byte).unwrap();
    std::thread::scope(|scope| {
        for t in 0..4u8 {
            let map = &map;
            scope.spawn(move || {
                for _ in 0..1000 {
                    let mut rz = map.read_zipper();
                    rz.descend_to_byte(t);
                }
            });
        }
        for _ in 0..50 {
            asm::disable(|site| site.event() == Event::descend_to_byte).unwrap();
            asm::enable(|site| site.event() == Event::descend_to_byte).unwrap();
        }
    });
    asm::disable_all().unwrap();
    asm::set_handler(None);
    let log = take_log();
    assert!(log.iter().all(|rec| matches!(rec, Rec::DescendToByte { byte, .. } if *byte < 4)));
}

fn put_back(log: Vec<Rec>) {
    let mut guard = LOG.lock().unwrap();
    let newer = core::mem::replace(&mut *guard, log);
    guard.extend(newer);
}
