//! Safe wrappers around the few spandsp parts the interop tests use: V.21
//! and V.22 with spandsp's own async framing, modem answer tones, and V.42.

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
type Status = unsafe extern "C" fn(*mut c_void, c_int);
type ToneReport = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int);
type V8Result = unsafe extern "C" fn(*mut c_void, *mut V8Parms);
type GetMsg = unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int;
type PutMsg = unsafe extern "C" fn(*mut c_void, *const u8, c_int);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct V8Parms {
    status: c_int,
    modem_connect_tone: c_int,
    send_ci: c_int,
    v92: c_int,
    call_function: c_int,
    modulations: std::ffi::c_uint,
    protocol: c_int,
    pstn_access: c_int,
    pcm_modem_availability: c_int,
    nsf: c_int,
    t66: c_int,
}

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

    fn v22bis_init(
        s: *mut c_void,
        bit_rate: c_int,
        guard: c_int,
        calling_party: bool,
        get_bit: GetBit,
        get_bit_user: *mut c_void,
        put_bit: PutBit,
        put_bit_user: *mut c_void,
    ) -> *mut c_void;
    fn v22bis_set_modem_status_handler(s: *mut c_void, handler: Status, user: *mut c_void);
    fn v22bis_tx(s: *mut c_void, amp: *mut i16, len: c_int) -> c_int;
    fn v22bis_rx(s: *mut c_void, amp: *const i16, len: c_int) -> c_int;
    fn v22bis_get_current_bit_rate(s: *mut c_void) -> c_int;
    fn v22bis_free(s: *mut c_void) -> c_int;

    fn v8_init(
        s: *mut c_void,
        calling_party: bool,
        parms: *mut V8Parms,
        result_handler: V8Result,
        user: *mut c_void,
    ) -> *mut c_void;
    fn v8_tx(s: *mut c_void, amp: *mut i16, max_len: c_int) -> c_int;
    fn v8_rx(s: *mut c_void, amp: *const i16, len: c_int) -> c_int;
    fn v8_free(s: *mut c_void) -> c_int;

    fn v42_init(
        s: *mut c_void,
        calling_party: bool,
        detect: bool,
        iframe_get: GetMsg,
        iframe_put: PutMsg,
        user: *mut c_void,
    ) -> *mut c_void;
    fn v42_set_status_callback(s: *mut c_void, callback: Status, user: *mut c_void);
    fn v42_restart(s: *mut c_void);
    fn v42_tx_bit(s: *mut c_void) -> c_int;
    fn v42_rx_bit(s: *mut c_void, bit: c_int);
    fn v42_free(s: *mut c_void) -> c_int;

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
const SIG_STATUS_TRAINING_SUCCEEDED: c_int = -4;
const SIG_STATUS_LINK_CONNECTED: c_int = -14;
const SIG_STATUS_LINK_DISCONNECTED: c_int = -15;
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
unsafe impl Send for V22bis {}
// SAFETY: as for `FskTx`.
unsafe impl Send for ToneTx {}
// SAFETY: as for `FskTx`.
unsafe impl Send for ToneRx {}
// SAFETY: as for `FskTx`.
unsafe impl Send for V42 {}

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

/// The guard tone a V.22 answering modem sends with its carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardTone {
    None = 0,
    Hz550 = 1,
    Hz1800 = 2,
}

#[derive(Default)]
struct V22Status {
    carrier: bool,
    trained: bool,
}

unsafe extern "C" fn v22_status(user: *mut c_void, status: c_int) {
    // SAFETY: `user` is the boxed `V22Status` owned by the `V22bis`.
    let state = unsafe { &mut *user.cast::<V22Status>() };
    match status {
        SIG_STATUS_CARRIER_UP => state.carrier = true,
        SIG_STATUS_CARRIER_DOWN => {
            state.carrier = false;
            state.trained = false;
        }
        SIG_STATUS_TRAINING_SUCCEEDED => state.trained = true,
        _ => {}
    }
}

/// spandsp's V.22bis modem, sending and receiving 8N1 characters through
/// V.14. Started at 1200 bit/s it is V.22, at 2400 bit/s it is V.22bis with
/// fallback to V.22. It has no answer tone of its own.
pub struct V22bis {
    modem: *mut c_void,
    framing: *mut c_void,
    bits: Box<Outgoing>,
    received: Box<Received>,
    status: Box<V22Status>,
}

impl V22bis {
    #[must_use]
    pub fn new(bit_rate: c_int, calling: bool, guard: GuardTone) -> Self {
        let mut bits = Box::<Outgoing>::default();
        let mut received = Box::<Received>::default();
        let mut status = Box::<V22Status>::default();
        let queue = ptr::addr_of_mut!(*bits).cast::<c_void>();
        let sink = ptr::addr_of_mut!(*received).cast::<c_void>();
        let report = ptr::addr_of_mut!(*status).cast::<c_void>();
        // SAFETY: spandsp allocates both states, `drop` frees them, the boxes outlive them.
        unsafe {
            let framing = async_rx_init(ptr::null_mut(), 8, 0, 1, true, put_byte, sink);
            let modem = v22bis_init(
                ptr::null_mut(),
                bit_rate,
                guard as c_int,
                calling,
                next_bit,
                queue,
                async_rx_put_bit,
                framing,
            );
            v22bis_set_modem_status_handler(modem, v22_status, report);
            Self {
                modem,
                framing,
                bits,
                received,
                status,
            }
        }
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
            v22bis_tx(self.modem, out.as_mut_ptr(), length(out.len()));
        }
    }

    pub fn process(&mut self, samples: &[i16]) {
        // SAFETY: `samples` is a valid buffer of the length passed.
        unsafe {
            v22bis_rx(self.modem, samples.as_ptr(), length(samples.len()));
        }
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.received.bytes
    }

    #[must_use]
    pub fn carrier(&self) -> bool {
        self.status.carrier
    }

    /// Whether the handshake is done and data flows.
    #[must_use]
    pub fn trained(&self) -> bool {
        self.status.trained
    }

    #[must_use]
    pub fn bit_rate(&self) -> c_int {
        // SAFETY: allocated in `new`.
        unsafe { v22bis_get_current_bit_rate(self.modem) }
    }
}

impl Drop for V22bis {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` and not freed before.
        unsafe {
            v22bis_free(self.modem);
            async_rx_free(self.framing);
        }
    }
}

const V8_STATUS_V8_CALL: c_int = 2;
const V8_CALL_V_SERIES: c_int = 6;
const V8_MOD_V21: std::ffi::c_uint = 1 << 1;
const V8_MOD_V22: std::ffi::c_uint = 1 << 2;

/// What a V.8 exchange agreed, as spandsp reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V8Outcome {
    pub agreed: bool,
    pub v22bis: bool,
    pub v21: bool,
}

unsafe extern "C" fn v8_result(user: *mut c_void, result: *mut V8Parms) {
    // SAFETY: `user` is the boxed slot owned by the `V8`, `result` is valid for this call.
    unsafe {
        *user.cast::<Option<V8Parms>>() = Some(*result);
    }
}

/// spandsp's V.8, offering V.22bis, V.21 or both. The answering side sends
/// `ANSam` with phase reversals itself.
pub struct V8 {
    state: *mut c_void,
    result: Box<Option<V8Parms>>,
}

// SAFETY: as for `FskTx`.
unsafe impl Send for V8 {}

impl V8 {
    #[must_use]
    pub fn new(calling: bool, v22bis: bool, v21: bool) -> Self {
        let mut result = Box::<Option<V8Parms>>::default();
        let mut parms = V8Parms {
            modem_connect_tone: if calling {
                0
            } else {
                AnswerTone::AnsamPr as c_int
            },
            v92: -1,
            call_function: V8_CALL_V_SERIES,
            modulations: if v22bis { V8_MOD_V22 } else { 0 } | if v21 { V8_MOD_V21 } else { 0 },
            nsf: -1,
            t66: -1,
            ..V8Parms::default()
        };
        let slot = ptr::addr_of_mut!(*result).cast::<c_void>();
        // SAFETY: spandsp allocates the state and copies `parms`, `drop` frees it, the slot outlives it.
        let state = unsafe { v8_init(ptr::null_mut(), calling, &raw mut parms, v8_result, slot) };
        Self { state, result }
    }

    pub fn render(&mut self, out: &mut [i16]) {
        out.fill(0);
        // SAFETY: `out` is a valid buffer of the length passed.
        unsafe {
            v8_tx(self.state, out.as_mut_ptr(), length(out.len()));
        }
    }

    pub fn process(&mut self, samples: &[i16]) {
        // SAFETY: `samples` is a valid buffer of the length passed.
        unsafe {
            v8_rx(self.state, samples.as_ptr(), length(samples.len()));
        }
    }

    /// The outcome once spandsp has reported one.
    #[must_use]
    pub fn outcome(&self) -> Option<V8Outcome> {
        self.result.map(|parms| V8Outcome {
            agreed: parms.status == V8_STATUS_V8_CALL,
            v22bis: parms.modulations & V8_MOD_V22 != 0,
            v21: parms.modulations & V8_MOD_V21 != 0,
        })
    }
}

impl Drop for V8 {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` and not freed before.
        unsafe {
            v8_free(self.state);
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

#[derive(Default)]
struct V42Data {
    outgoing: VecDeque<u8>,
    received: Vec<u8>,
    connected: bool,
}

unsafe extern "C" fn v42_get(user: *mut c_void, msg: *mut u8, max_len: c_int) -> c_int {
    // SAFETY: `user` is the boxed `V42Data` owned by the `V42`.
    let data = unsafe { &mut *user.cast::<V42Data>() };
    let n = data
        .outgoing
        .len()
        .min(usize::try_from(max_len).unwrap_or(0));
    for (i, byte) in data.outgoing.drain(..n).enumerate() {
        // SAFETY: spandsp gave room for `max_len` octets, and `i` is below it.
        unsafe { msg.add(i).write(byte) };
    }
    length(n)
}

unsafe extern "C" fn v42_put(user: *mut c_void, msg: *const u8, len: c_int) {
    // SAFETY: `user` is the boxed `V42Data` owned by the `V42`.
    let data = unsafe { &mut *user.cast::<V42Data>() };
    if let Ok(len) = usize::try_from(len)
        && len > 0
    {
        // SAFETY: spandsp passes `len` valid octets.
        data.received
            .extend_from_slice(unsafe { std::slice::from_raw_parts(msg, len) });
    }
}

unsafe extern "C" fn v42_status(user: *mut c_void, status: c_int) {
    // SAFETY: `user` is the boxed `V42Data` owned by the `V42`.
    let data = unsafe { &mut *user.cast::<V42Data>() };
    match status {
        SIG_STATUS_LINK_CONNECTED => data.connected = true,
        SIG_STATUS_LINK_DISCONNECTED => data.connected = false,
        _ => {}
    }
}

/// spandsp's V.42, over bits with no modulation under it.
pub struct V42 {
    v42: *mut c_void,
    data: Box<V42Data>,
}

impl V42 {
    /// With `detect`, it starts with the detection phase, else with LAPM.
    #[must_use]
    pub fn new(calling_party: bool, detect: bool) -> Self {
        let mut data = Box::<V42Data>::default();
        let user = ptr::addr_of_mut!(*data).cast::<c_void>();
        // SAFETY: spandsp allocates the state, `drop` frees it, the data outlives it.
        let v42 = unsafe {
            let v42 = v42_init(
                ptr::null_mut(),
                calling_party,
                detect,
                v42_get,
                v42_put,
                user,
            );
            v42_set_status_callback(v42, v42_status, user);
            v42_restart(v42);
            v42
        };
        Self { v42, data }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.data.outgoing.extend(bytes);
    }

    #[must_use]
    pub fn tx_bit(&mut self) -> bool {
        // SAFETY: allocated in `new`.
        unsafe { v42_tx_bit(self.v42) != 0 }
    }

    pub fn rx_bit(&mut self, bit: bool) {
        // SAFETY: allocated in `new`.
        unsafe { v42_rx_bit(self.v42, c_int::from(bit)) }
    }

    #[must_use]
    pub fn received(&self) -> &[u8] {
        &self.data.received
    }

    #[must_use]
    pub fn connected(&self) -> bool {
        self.data.connected
    }
}

impl Drop for V42 {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` and not freed before.
        unsafe {
            v42_free(self.v42);
        }
    }
}
