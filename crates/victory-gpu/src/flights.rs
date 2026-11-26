//! Submissions in flight (`crate::host::late`, `crate::host::relay`),
//! through the title's own calls: queues, lists, copies between title
//! buffers, fences, releases. The switches are read once per process, so
//! each mode runs this file's scenario in a child process of the test
//! binary with its variables set ([`modes`]); the scenario itself is an
//! ignored test that does nothing unless it is such a child.
//!
//! What it checks, in every mode:
//! - read after write across submissions: a chain of copies, each
//!   submission reading the buffer the one before it wrote;
//! - write after write: two submissions write the same buffer, the later
//!   one's bytes stay;
//! - lifetime: a title buffer released right after the submission that
//!   reads it, while that submission may still be in the air;
//! - synchronisation: the title's fence reaches its value only once all of
//!   it is in title memory, and no device is lost on the way.

use crate::object::This;
use crate::resource::tests::title_buffer;

/// The method at byte `offset` of a title object's table.
///
/// # Safety
/// `object` is one of our title objects and `F` the method's type.
unsafe fn method<F: Copy>(object: This, offset: usize) -> F {
    // SAFETY: a title object starts with its table; the caller names the type.
    unsafe {
        let table = *(object as *const *const u8);
        std::ptr::read(table.add(offset).cast::<F>())
    }
}

type Close = extern "system" fn(This) -> u32;
type CopyBuffer = extern "system" fn(This, This, u64, This, u64, u64);
type Execute = extern "system" fn(This, u32, *const This);
type Signal = extern "system" fn(This, This, u64) -> i32;
type Release = extern "system" fn(This) -> u32;

use crate::object::guid as iid;

/// A title list ready to record, on an allocator of its own.
fn new_list() -> This {
    let null = std::ptr::null();
    let allocator_iid = iid(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
    let list_iid = iid(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
    let (mut allocator, mut list) = (null, null);
    assert_eq!(crate::list::create_allocator(null, 0, &allocator_iid, &mut allocator), 0);
    assert_eq!(crate::list::create_list(null, 0, 0, allocator, null, &list_iid, &mut list), 0);
    list
}

/// One submission of one list holding `copies` (to, from, bytes).
fn submit(queue: This, copies: &[(This, This, u64)]) {
    let list = new_list();
    for &(to, from, bytes) in copies {
        // SAFETY: CopyBufferRegion of a title list.
        (unsafe { method::<CopyBuffer>(list, 0x78) })(list, to, 0, from, 0, bytes);
    }
    // SAFETY: Close of a title list, ExecuteCommandLists of a title queue.
    unsafe {
        method::<Close>(list, 0x48)(list);
        method::<Execute>(queue, 0x50)(queue, 1, &list);
    }
}

fn bytes_of(buffer: (This, u64), len: usize) -> &'static [u8] {
    // SAFETY: a title buffer's own memory, `len` bytes or more.
    unsafe { std::slice::from_raw_parts(buffer.1 as *const u8, len) }
}

/// The scenario, in a child process (`SIXTH_FLIGHTS_CHILD=1`).
#[test]
#[ignore = "run by `submissions_in_flight_keep_order_and_lifetime` in a child process"]
fn flights_child() {
    if std::env::var_os("SIXTH_FLIGHTS_CHILD").is_none() || crate::host::host().is_none() {
        return;
    }
    const LEN: usize = 64 << 10;
    const CHAIN: usize = 48;
    let null = std::ptr::null();
    let queue_iid = iid(0x0ec870a6, 0x5d7e, 0x4c22, [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed]);
    let fence_iid = iid(0x0a753dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);
    let mut queue = null;
    let desc = [0u32; 4];
    assert_eq!(crate::queue::create(null, desc.as_ptr().cast(), &queue_iid, &mut queue), 0);
    let mut fence = null;
    assert_eq!(crate::fence::create(null, 0, 0, &fence_iid, &mut fence), 0);

    // Read after write: X0 -> X1 -> ... -> X(CHAIN), one submission each.
    let chain: Vec<(This, u64)> = (0..=CHAIN).map(|_| title_buffer(LEN as u64, 0)).collect();
    // SAFETY: the first buffer's own memory.
    let first = unsafe { std::slice::from_raw_parts_mut(chain[0].1 as *mut u8, LEN) };
    for (i, b) in first.iter_mut().enumerate() {
        *b = (i * 7 % 251) as u8;
    }
    let pattern = first.to_vec();
    for i in 0..CHAIN {
        submit(queue, &[(chain[i + 1].0, chain[i].0, LEN as u64)]);
        // Lifetime: every eighth source is released at once, while the
        // submission that reads it may still be in the air.
        if i % 8 == 1 {
            // SAFETY: Release of a title buffer we made.
            (unsafe { method::<Release>(chain[i].0, 0x10) })(chain[i].0);
        }
    }
    // Write after write: P then Q into Y, in two submissions.
    let (p, q, y) = (title_buffer(LEN as u64, 0), title_buffer(LEN as u64, 0), title_buffer(LEN as u64, 0));
    // SAFETY: the buffers' own memory.
    unsafe {
        std::slice::from_raw_parts_mut(p.1 as *mut u8, LEN).fill(0x11);
        std::slice::from_raw_parts_mut(q.1 as *mut u8, LEN).fill(0x22);
    }
    submit(queue, &[(y.0, p.0, LEN as u64)]);
    submit(queue, &[(y.0, q.0, LEN as u64)]);
    // The title's fence, behind all of it.
    // SAFETY: Signal of a title queue.
    assert_eq!(unsafe { method::<Signal>(queue, 0x70) }(queue, fence, 1), 0);
    let began = std::time::Instant::now();
    while crate::fence::completed(fence) < 1 {
        assert!(began.elapsed().as_secs() < 20, "the fence never reached its value");
        std::thread::yield_now();
    }
    assert_eq!(crate::descriptor::host::device_removed(), None, "the device was lost");
    let last = bytes_of(chain[CHAIN], LEN);
    let wrong = last.iter().zip(&pattern).filter(|(a, b)| a != b).count();
    assert_eq!(wrong, 0, "the chain's last buffer differs in {wrong} bytes");
    assert!(bytes_of(y, LEN).iter().all(|&b| b == 0x22), "the later write stays");
    // Copies in the card's memory (title buffers of 16 MiB or more), as in
    // the failure seen on another card with the late wait: a ping-pong of
    // copies between two of them, and the title writing the source between
    // its own fence waits (write after read, done right).
    const BIG: usize = 16 << 20;
    let (s, d1, d2) = (title_buffer(BIG as u64, 0), title_buffer(BIG as u64, 0), title_buffer(BIG as u64, 0));
    // SAFETY: the source's own memory.
    let source = unsafe { std::slice::from_raw_parts_mut(s.1 as *mut u8, BIG) };
    for (value, round) in (2u64..).zip(0..6u8) {
        // The title writes a few scattered pages of the source, after the
        // fence said the last round's copies are done.
        for page in (0..BIG / 4096).step_by(97) {
            source[page * 4096 + usize::from(round)] = round.wrapping_mul(31).wrapping_add(page as u8);
        }
        submit(queue, &[(d1.0, s.0, BIG as u64)]);
        submit(queue, &[(d2.0, d1.0, BIG as u64)]);
        submit(queue, &[(d1.0, d2.0, BIG as u64)]);
        // SAFETY: Signal of a title queue.
        assert_eq!((unsafe { method::<Signal>(queue, 0x70) })(queue, fence, value), 0);
        let began = std::time::Instant::now();
        while crate::fence::completed(fence) < value {
            assert!(began.elapsed().as_secs() < 30, "round {round}: the fence never reached {value}");
            std::thread::yield_now();
        }
        assert_eq!(crate::descriptor::host::device_removed(), None, "round {round}: the device was lost");
        for (name, d) in [("first", d1), ("second", d2)] {
            let got = bytes_of(d, BIG);
            let wrong = got.iter().zip(source.iter()).filter(|(a, b)| a != b).count();
            assert_eq!(wrong, 0, "round {round}: the {name} copy differs from the source in {wrong} bytes");
        }
    }
    let carded = crate::shadow::find(d1.1, 256).is_some_and(|c| c.carded());
    println!("FLIGHTS-CARD {carded}");
    // The mode asked for is the one that ran: in a late mode, submissions
    // went to the card without a wait.
    let (sent, settles) = crate::host::late_counts();
    let late_asked = std::env::var_os("VICTORY_LATE_WAIT").is_some() || std::env::var_os("VICTORY_RELAY").is_some();
    assert_eq!(crate::host::late(), late_asked, "the late wait is as asked");
    if std::env::var_os("VICTORY_RELAY").is_some() {
        assert!(crate::host::relay(), "the relay is on");
    }
    if late_asked {
        assert!(sent >= CHAIN as u64, "only {sent} submissions sent without a wait");
    }
    println!("FLIGHTS-CHILD-OK sent ahead {sent}, settles {settles}, late {}, relay {}", crate::host::late(), crate::host::relay());
}

/// The modes the scenario runs in, each a child process.
fn modes() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("synchronous", vec![]),
        ("late wait 1", vec![("VICTORY_LATE_WAIT", "1")]),
        ("late wait 2", vec![("VICTORY_LATE_WAIT", "2")]),
        ("late wait 3", vec![("VICTORY_LATE_WAIT", "3")]),
        ("relay 1", vec![("VICTORY_RELAY", "1")]),
        ("relay 2", vec![("VICTORY_RELAY", "2")]),
    ]
}

/// Every mode of submissions in flight keeps the title's order, its
/// buffers' lifetime and its fence's meaning (the scenario above).
#[test]
fn submissions_in_flight_keep_order_and_lifetime() {
    if crate::host::host().is_none() {
        return; // no host GPU here
    }
    let exe = std::env::current_exe().expect("the test binary");
    let mut failed = Vec::new();
    for (name, vars) in modes() {
        let mut child = std::process::Command::new(&exe);
        child
            .args(["--exact", "flights::flights_child", "--ignored", "--nocapture", "--test-threads", "1"])
            .env("SIXTH_FLIGHTS_CHILD", "1")
            .env_remove("VICTORY_LATE_WAIT")
            .env_remove("VICTORY_RELAY")
            .env_remove("VICTORY_GPU_TIMING");
        for (k, v) in vars {
            child.env(k, v);
        }
        let out = child.output().expect("the child ran");
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || !text.contains("FLIGHTS-CHILD-OK") {
            let why: String = text.lines().filter(|l| l.contains("panicked") || l.contains("assert")).collect::<Vec<_>>().join(" | ");
            failed.push(format!("{name}: {why}"));
        }
    }
    assert!(failed.is_empty(), "modes that failed: {failed:#?}");
}
