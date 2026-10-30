//! Exercises several zipper types so their USDT probes fire.  Pair it with `usdt_demo.bt`:
//!
//! ```sh
//! cargo build --release --example usdt_demo --features trace_usdt
//! sudo bpftrace -e "$(cat examples/usdt_demo.bt)" -c target/release/examples/usdt_demo
//! ```
//!
//! (`-e "$(cat ...)"` rather than the file path works around a bpftrace 0.25 abort, see the script.)
//!
//! Arguments: `usdt_demo [keys] [delay_ms]`.  `keys` (default 4) is how many keys to insert.
//! `delay_ms` (default 0) sleeps before starting, which gives a tracer time to attach when it is
//! started separately instead of with `-c`.

use pathmap::PathMap;
use pathmap::zipper::*;

fn main() {
    let mut args = std::env::args().skip(1);
    let keys: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    let delay_ms: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(delay_ms));

    // 1. A write zipper inserts keys
    let mut map = PathMap::<u64>::new();
    {
        let mut wz = map.write_zipper();
        for i in 0..keys {
            wz.descend_to(format!("fruit:{i}"));
            wz.set_val(100 + i as u64);
            wz.reset();
        }
        println!("write zipper uid {}", wz.trace_uid());
    }

    // 2. A read zipper walks every value, and a clone of it gets its own uid
    let mut rz = map.read_zipper();
    let mut sum = 0;
    while rz.to_next_val() {
        sum += *rz.val().unwrap();
    }
    rz.reset();
    rz.descend_to(b"fruit:");
    let mut rz_clone = rz.clone();
    rz_clone.descend_to_byte(b'0');
    println!("read zipper uid {}, clone uid {}, sum {sum}", rz.trace_uid(), rz_clone.trace_uid());

    // 3. A write zipper grafts the read zipper's subtrie into a second map
    let mut copy = PathMap::<u64>::new();
    {
        let mut wz = copy.write_zipper_at_path(b"copied:");
        wz.graft(&rz);
        println!("graft into uid {} from uid {}", wz.trace_uid(), rz.trace_uid());
    }
    drop(rz_clone);
    drop(rz);

    // 4. Forking a read zipper off a write zipper links child to parent (zipper_derive)
    {
        let mut wz = map.write_zipper();
        wz.descend_to(b"fruit:");
        let fork = wz.fork_read_zipper().unwrap();
        println!("fork uid {} of write zipper uid {}, {} values", fork.trace_uid(), wz.trace_uid(), fork.val_count());
    }

    // 5. Composite zippers have their own uids and trace their own methods
    {
        let mut pz = PrefixZipper::new(b"basket/", map.read_zipper());
        pz.descend_to(b"basket/fruit:1");
        println!("prefix zipper uid {} at {:?}", pz.trace_uid(), pz.val());

        let mut product = ProductZipper::new(map.read_zipper(), [copy.read_zipper()]);
        product.descend_to(b"fruit:0");
        println!("product zipper uid {}, {} values", product.trace_uid(), product.val_count());
    }
}
