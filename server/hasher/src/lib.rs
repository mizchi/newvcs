//! BLAKE3 for a host that keeps the state itself.
//!
//! There is one hasher here and one buffer, both static, and nothing is allocated. The
//! hasher is plain bytes with no pointers in it, so a host hashing several contents at
//! once copies `state_size()` bytes out of `state()` and back in when it changes between
//! them. A hasher the host forgets about costs nothing here.
#![no_std]

use blake3::Hasher;
use core::mem::{size_of, MaybeUninit};
use core::ptr::addr_of_mut;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// Bytes the host may write at `input()` before each `update`.
const INPUT: usize = 1 << 16;

static mut BUFFER: [u8; INPUT] = [0; INPUT];
static mut OUT: [u8; 32] = [0; 32];
static mut STATE: MaybeUninit<Hasher> = MaybeUninit::uninit();

#[no_mangle]
pub extern "C" fn input() -> *mut u8 {
    addr_of_mut!(BUFFER).cast()
}

#[no_mangle]
pub extern "C" fn input_size() -> usize {
    INPUT
}

#[no_mangle]
pub extern "C" fn state() -> *mut u8 {
    addr_of_mut!(STATE).cast()
}

#[no_mangle]
pub extern "C" fn state_size() -> usize {
    size_of::<Hasher>()
}

/// Make the state that of a hasher that has seen nothing.
#[no_mangle]
pub extern "C" fn init() {
    // One thread, and no reference to the state outlives a call.
    unsafe { (*addr_of_mut!(STATE)).write(Hasher::new()) };
}

/// Hash the first `length` bytes of the buffer. The state is one `init` wrote, or a copy of it.
#[no_mangle]
pub extern "C" fn update(length: usize) {
    let (hasher, buffer) = unsafe { ((*addr_of_mut!(STATE)).assume_init_mut(), &*addr_of_mut!(BUFFER)) };
    if let Some(bytes) = buffer.get(..length) {
        hasher.update(bytes);
    }
}

/// Where the 32 bytes of the hash of everything seen are. The state is left as it was.
#[no_mangle]
pub extern "C" fn finish() -> *const u8 {
    let (hasher, out) = unsafe { ((*addr_of_mut!(STATE)).assume_init_ref(), &mut *addr_of_mut!(OUT)) };
    *out = *hasher.finalize().as_bytes();
    out.as_ptr()
}
