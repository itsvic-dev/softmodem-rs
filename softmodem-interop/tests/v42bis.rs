// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our V.42bis codec against spandsp's, one direction at a time.

use softmodem_interop::{CompressionMode, V42bis};
use softmodem_link::v42bis::{Decoder, Encoder, Parameters};

const SIZES: [(u16, u8); 3] = [(512, 6), (2048, 32), (4096, 250)];
const MODES: [CompressionMode; 3] = [
    CompressionMode::Dynamic,
    CompressionMode::Always,
    CompressionMode::Never,
];

fn text(len: usize) -> Vec<u8> {
    let words = [
        "the ", "modem ", "sends ", "a ", "carrier ", "and ", "data ", "over ", "line ", "to ",
        "ppp\r\n", "\0", "3", "f", "\u{99}",
    ];
    let mut seed = 7u32;
    let mut out = Vec::new();
    while out.len() < len {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        out.extend(words[(seed >> 16) as usize % words.len()].bytes());
    }
    out.truncate(len);
    out
}

fn noise(len: usize) -> Vec<u8> {
    let mut seed = 1u32;
    (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed.to_le_bytes()[0]
        })
        .collect()
}

fn data() -> Vec<u8> {
    [
        text(20_000),
        noise(4000),
        text(10_000),
        vec![b'C'; 3000],
        noise(3000),
        text(5000),
    ]
    .concat()
}

fn parameters((codewords, max_string): (u16, u8)) -> Parameters {
    Parameters {
        codewords,
        max_string,
    }
}

#[test]
fn we_decode_what_spandsp_encodes() {
    let data = data();
    for size in SIZES {
        for mode in MODES {
            let mut theirs = V42bis::new(size.0, size.1, mode);
            let mut ours = Decoder::new(parameters(size));
            let mut out = Vec::new();
            for chunk in data.chunks(333) {
                let wire = theirs.compress(chunk);
                ours.decode(&wire, &mut out)
                    .unwrap_or_else(|e| panic!("{size:?} {mode:?}: {e:?} at {}", out.len()));
            }
            assert!(
                out == data,
                "{size:?} {mode:?}: {} of {}",
                out.len(),
                data.len()
            );
        }
    }
}

#[test]
fn spandsp_decodes_what_we_encode() {
    let data = data();
    for size in SIZES {
        let mut ours = Encoder::new(parameters(size));
        let mut theirs = V42bis::new(size.0, size.1, CompressionMode::Dynamic);
        let mut out = Vec::new();
        let mut sent = 0;
        for chunk in data.chunks(333) {
            let mut wire = ours.encode(chunk);
            wire.extend(ours.flush());
            sent += wire.len();
            out.extend(theirs.decompress(&wire));
        }
        assert!(out == data, "{size:?}: {} of {}", out.len(), data.len());
        assert!(
            sent * 10 < data.len() * 8,
            "{size:?}: {sent} for {}",
            data.len()
        );
    }
}
