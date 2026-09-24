//! White noise on the line from some time into the call, to make one end
//! renegotiate or retrain.

const FRAME_SECONDS: f64 = 0.02;

/// Adds noise of a given RMS to every frame from a given frame on.
#[derive(Debug)]
pub struct Noise {
    from_frame: u64,
    rms: f64,
    frames: u64,
    state: u64,
    pub to_softmodem: bool,
    pub to_slmodemd: bool,
}

impl Noise {
    /// From `SOFTMODEM_NOISE_AFTER`, in seconds after the answer, and
    /// `SOFTMODEM_NOISE_RMS`, in linear sample units. None without both.
    /// `SOFTMODEM_NOISE_WAY` of `to-softmodem` or `to-slmodemd` keeps it to
    /// one direction.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "seconds into a call"
    )]
    pub fn from_env() -> Option<Self> {
        let after: f64 = std::env::var("SOFTMODEM_NOISE_AFTER").ok()?.parse().ok()?;
        let rms: f64 = std::env::var("SOFTMODEM_NOISE_RMS").ok()?.parse().ok()?;
        let way = std::env::var("SOFTMODEM_NOISE_WAY").unwrap_or_default();
        Some(Self {
            from_frame: (after / FRAME_SECONDS) as u64,
            rms,
            frames: 0,
            state: 0x2545_F491_4F6C_DD1D,
            to_softmodem: way != "to-slmodemd",
            to_slmodemd: way != "to-softmodem",
        })
    }

    /// Whether the noise has started. Call once per frame from the softmodem.
    pub fn tick(&mut self) -> bool {
        self.frames += 1;
        self.frames == self.from_frame
    }

    fn uniform(&mut self) -> f64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        f64::from(u32::try_from(self.state >> 32).unwrap_or(0)) / f64::from(u32::MAX) - 0.5
    }

    #[expect(clippy::cast_possible_truncation, reason = "clamped to i16")]
    pub fn add(&mut self, frame: &mut [i16]) {
        if self.frames < self.from_frame {
            return;
        }
        // Four uniforms have a variance of 1/3.
        let gain = self.rms * 3f64.sqrt();
        for sample in frame {
            let noise = (0..4).map(|_| self.uniform()).sum::<f64>() * gain;
            *sample = (f64::from(*sample) + noise).clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}
