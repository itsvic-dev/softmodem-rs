//! Safe wrappers around the few spandsp parts the interop tests use: V.21
//! with spandsp's own async framing, and modem answer tones.

use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

#[repr(C)]
struct FskSpec {
    name: *const c_char,
    freq_zero: c_int,
    freq_one: c_int,
    tx_level: c_int,
    min_level: c_int,
    baud_rate: c_int,
}

type GetBit = unsafe extern "C" fn(*mut c_void) -> c_int;
type PutBit = unsafe extern "C" fn(*mut c_void, c_int);
type PutByte = unsafe extern "C" fn(*mut c_void, c_int);
type ToneReport = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int);

#[link(name = "spandsp")]
unsafe extern "C" {
    static preset_fsk_specs: [FskSpec; 0];

    fn fsk_tx_init(
        s: *mut c_void,
        spec: *const FskSpec,
        get_bit: GetBit,
        user: *mut c_void,
    ) -> *mut c_void;
    fn fsk_tx(s: *mut c_void, amp: *mut i16, len: c_int) -> c_int;
    fn fsk_tx_free(s: *mut c_void) -> c_int;
    fn fsk_rx_init(
        s: *mut c_void,
        spec: *const FskSpec,
        framing_mode: c_int,
        put_bit: PutBit,
        user: *mut c_void,
    ) -> *mut c_void;
    fn fsk_rx(s: *mut c_void, amp: *const i16, len: c_int) -> c_int;
    fn fsk_rx_free(s: *mut c_void) -> c_int;

    fn async_rx_init(
        s: *mut c_void,
        data_bits: c_int,
        parity: c_int,
        stop_bits: c_int,
        use_v14: bool,
        put_byte: PutByte,
        user: *mut c_void,
    ) -> *mut c_void;
    fn async_rx_put_bit(user: *mut c_void, bit: c_int);
    fn async_rx_free(s: *mut c_void) -> c_int;

    fn modem_connect_tones_tx_init(s: *mut c_void, tone: c_int) -> *mut c_void;
    fn modem_connect_tones_tx(s: *mut c_void, amp: *mut i16, len: c_int) -> c_int;
    fn modem_connect_tones_tx_free(s: *mut c_void) -> c_int;
    fn modem_connect_tones_rx_init(
        s: *mut c_void,
        tone: c_int,
        report: Option<ToneReport>,
        user: *mut c_void,
    ) -> *mut c_void;
    fn modem_connect_tones_rx(s: *mut c_void, amp: *const i16, len: c_int) -> c_int;
    fn modem_connect_tones_rx_get(s: *mut c_void) -> c_int;
    fn modem_connect_tones_rx_free(s: *mut c_void) -> c_int;
}

const SIG_STATUS_CARRIER_UP: c_int = -2;
const SIG_STATUS_CARRIER_DOWN: c_int = -1;
const FSK_FRAME_MODE_ASYNC: c_int = 0;

/// One of spandsp's preset FSK channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FskChannel {
    V21Originate = 0,
    V21Answer = 1,
    Bell103Originate = 4,
    Bell103Answer = 5,
}

fn spec(channel: FskChannel) -> *const FskSpec {
    // SAFETY: the preset table has more entries than any `FskChannel` index.
    unsafe {
        ptr::addr_of!(preset_fsk_specs)
            .cast::<FskSpec>()
            .add(channel as usize)
    }
}

// SAFETY: each wrapper owns its spandsp state alone, and spandsp has no thread affinity.
unsafe impl Send for FskTx {}
// SAFETY: as for `FskTx`.
unsafe impl Send for FskRx {}
// SAFETY: as for `FskTx`.
unsafe impl Send for ToneTx {}
// SAFETY: as for `FskTx`.
unsafe impl Send for ToneRx {}

fn length(samples: usize) -> c_int {
    c_int::try_from(samples).expect("a frame fits in a c_int")
}

/// spandsp's V.21 transmitter, sending 8N1 characters and idling on mark.
///
/// The framing is done here: spandsp's own async transmitter ends the
/// carrier when it runs out of data, where a modem idles on mark.
pub struct FskTx {
    fsk: *mut c_void,
    bits: Box<Outgoing>,
}

#[derive(Default)]
struct Outgoing(VecDeque<c_int>);

unsafe extern "C" fn next_bit(user: *mut c_void) -> c_int {
    // SAFETY: `user` is the boxed queue owned by the `FskTx`.
    let bits = unsafe { &mut *user.cast::<Outgoing>() };
    bits.0.pop_front().unwrap_or(1)
}

impl FskTx {
    #[must_use]
    pub fn new(channel: FskChannel) -> Self {
        let mut bits = Box::<Outgoing>::default();
        let queue = ptr::addr_of_mut!(*bits).cast::<c_void>();
        // SAFETY: spandsp allocates the state, `drop` frees it, the queue outlives it.
        let fsk = unsafe { fsk_tx_init(ptr::null_mut(), spec(channel), next_bit, queue) };
        Self { fsk, bits }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.bits.0.push_back(0);
            self.bits
                .0
                .extend((0..8).map(|i| c_int::from(byte >> i & 1)));
            self.bits.0.push_back(1);
        }
    }

    #[must_use]
    pub fn pending(&self) -> usize {
        self.bits.0.len()
    }

    pub fn render(&mut self, out: &mut [i16]) {
        // SAFETY: `out` is a valid buffer of the length passed.
        unsafe {
            fsk_tx(self.fsk, out.as_mut_ptr(), length(out.len()));
        }
    }
}

impl Drop for FskTx {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` and not freed before.
        unsafe {
            fsk_tx_free(self.fsk);
        }
    }
}

#[derive(Default)]
struct Received {
    bytes: Vec<u8>,
    carrier: bool,
}

/// spandsp's V.21 receiver, with its own 8N1 framing.
pub struct FskRx {
    fsk: *mut c_void,
    framing: *mut c_void,
    received: Box<Received>,
}

unsafe extern "C" fn put_byte(user: *mut c_void, byte: c_int) {
    // SAFETY: `user` is the boxed `Received` owned by the `FskRx`.
    let received = unsafe { &mut *user.cast::<Received>() };
    match byte {
        SIG_STATUS_CARRIER_UP => received.carrier = true,
        SIG_STATUS_CARRIER_DOWN => received.carrier = false,
        byte => {
            if let Ok(byte) = u8::try_from(byte) {
                received.bytes.push(byte);
            }
        }
    }
}

impl FskRx {
    #[must_use]
    pub fn new(channel: FskChannel) -> Self {
        let mut received = Box::<Received>::default();
        let sink = ptr::addr_of_mut!(*received).cast::<c_void>();
        // SAFETY: spandsp allocates both states, `drop` frees them, the sink outlives them.
        unsafe {
            let framing = async_rx_init(ptr::null_mut(), 8, 0, 1, false, put_byte, sink);
            let fsk = fsk_rx_init(
                ptr::null_mut(),
                spec(channel),
                FSK_FRAME_MODE_ASYNC,
                async_rx_put_bit,
                framing,
            );
            Self {
                fsk,
                framing,
                received,
            }
        }
    }

    pub fn process(&mut self, samples: &[i16]) {
        // SAFETY: `samples` is a valid buffer of the length passed.
        unsafe {
            fsk_rx(self.fsk, samples.as_ptr(), length(samples.len()));
        }
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.received.bytes
    }

    #[must_use]
    pub fn carrier(&self) -> bool {
        self.received.carrier
    }
}

impl Drop for FskRx {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` and not freed before.
        unsafe {
            fsk_rx_free(self.fsk);
            async_rx_free(self.framing);
        }
    }
}

/// The answer tones spandsp knows, with its own numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerTone {
    /// V.25 2100 Hz.
    Ans = 2,
    /// V.25 2100 Hz with phase reversals, which disable echo cancellers.
    AnsPr = 3,
    /// V.8 2100 Hz amplitude modulated at 15 Hz.
    Ansam = 4,
    /// V.8 `ANSam` with phase reversals, what a modern answering modem sends.
    AnsamPr = 5,
    /// Bell 2225 Hz.
    Bell = 8,
}

impl AnswerTone {
    fn from_code(code: c_int) -> Option<Self> {
        [
            Self::Ans,
            Self::AnsPr,
            Self::Ansam,
            Self::AnsamPr,
            Self::Bell,
        ]
        .into_iter()
        .find(|tone| *tone as c_int == code)
    }
}

pub struct ToneTx(*mut c_void);

impl ToneTx {
    #[must_use]
    pub fn new(tone: AnswerTone) -> Self {
        // SAFETY: allocated by spandsp and freed in `drop`.
        Self(unsafe { modem_connect_tones_tx_init(ptr::null_mut(), tone as c_int) })
    }

    pub fn render(&mut self, out: &mut [i16]) {
        // SAFETY: `out` is a valid buffer of the length passed.
        unsafe {
            modem_connect_tones_tx(self.0, out.as_mut_ptr(), length(out.len()));
        }
    }
}

impl Drop for ToneTx {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` and not freed before.
        unsafe {
            modem_connect_tones_tx_free(self.0);
        }
    }
}

/// spandsp's detector for one kind of answer tone.
pub struct ToneRx(*mut c_void);

impl ToneRx {
    #[must_use]
    pub fn new(tone: AnswerTone) -> Self {
        // SAFETY: allocated by spandsp and freed in `drop`.
        Self(unsafe {
            modem_connect_tones_rx_init(ptr::null_mut(), tone as c_int, None, ptr::null_mut())
        })
    }

    pub fn process(&mut self, samples: &[i16]) {
        // SAFETY: `samples` is a valid buffer of the length passed.
        unsafe {
            modem_connect_tones_rx(self.0, samples.as_ptr(), length(samples.len()));
        }
    }

    /// The tone heard so far, if any.
    #[must_use]
    pub fn detected(&self) -> Option<AnswerTone> {
        // SAFETY: allocated in `new`.
        AnswerTone::from_code(unsafe { modem_connect_tones_rx_get(self.0) })
    }
}

impl Drop for ToneRx {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` and not freed before.
        unsafe {
            modem_connect_tones_rx_free(self.0);
        }
    }
}
