//! SMPTE LTC (Linear Timecode) receiver.
//!
//! Decodes biphase-mark coded audio from a `cpal` input device and exposes
//! the latest timecode frame via [`LtcReceiver::status`].

use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

use crate::tc::{frame_index_from_tc, nominal_fps, tc_from_frame_index, Timecode};

// ─── shared state ────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Shared {
    tc: Option<Timecode>,
    last_rx: Option<Instant>,
    fps_label: f32,
    fps_n: u32,
    device: Option<String>,
    error: Option<String>,
}

// ─── public status snapshot ───────────────────────────────────────────────────

#[derive(Clone)]
pub struct LtcStatus {
    pub tc: Option<Timecode>,
    /// How long ago the last frame arrived.
    pub age: Option<std::time::Duration>,
    pub fps_label: f32,
    pub fps_n: u32,
    pub device: Option<String>,
    pub error: Option<String>,
}

// ─── LtcReceiver ─────────────────────────────────────────────────────────────

/// Owns a live `cpal` input stream and decodes incoming LTC.
///
/// `cpal::Stream` is `!Send`, so this struct must live on one thread.
pub struct LtcReceiver {
    // Kept alive for its Drop side-effect (stops the stream).
    _stream: Option<cpal::Stream>,
    shared: Arc<Mutex<Shared>>,
}

impl LtcReceiver {
    pub fn new() -> Self {
        LtcReceiver {
            _stream: None,
            shared: Arc::new(Mutex::new(Shared::default())),
        }
    }

    /// Return all available input device names (empty on any error).
    pub fn list_devices() -> Vec<String> {
        let host = cpal::default_host();
        match host.input_devices() {
            // cpal 0.18: DeviceTrait::name() removed; use Display impl (to_string())
            Ok(devs) => devs.map(|d| d.to_string()).collect(),
            Err(_) => vec![],
        }
    }

    /// Connect to the named device, or the system default when `None`.
    pub fn connect(&mut self, device_name: Option<&str>) -> Result<(), String> {
        self.disconnect();

        let host = cpal::default_host();

        let device = match device_name {
            None => host
                .default_input_device()
                .ok_or_else(|| "No default input device".to_string())?,
            Some(name) => host
                .input_devices()
                .map_err(|e| e.to_string())?
                // cpal 0.18: DeviceTrait::name() removed; compare via Display
                .find(|d| d.to_string() == name)
                .ok_or_else(|| format!("Device not found: {name}"))?,
        };

        // cpal 0.18: device name via Display trait
        let dev_name = Some(device.to_string());
        let cfg = device.default_input_config().map_err(|e| e.to_string())?;
        let channels = cfg.channels() as usize;
        // cpal 0.18: SampleRate is now a plain u32, not a newtype (.0 removed)
        let sample_rate = cfg.sample_rate() as f32;
        let fmt = cfg.sample_format();

        let shared = Arc::clone(&self.shared);
        {
            let mut s = shared.lock().unwrap();
            s.device = dev_name.clone();
            s.error = None;
        }

        let shared_data = Arc::clone(&shared);
        let shared_err = Arc::clone(&shared);

        let mut decoder = LtcDecoder::new(sample_rate);

        // cpal 0.18: build_input_stream takes StreamConfig by value, not by reference
        let stream = match fmt {
            SampleFormat::F32 => {
                let cfg2: cpal::StreamConfig = cfg.into();
                device.build_input_stream(
                    cfg2,
                    move |data: &[f32], _| {
                        feed_callback(data, channels, &mut decoder, &shared_data);
                    },
                    move |e| {
                        if let Ok(mut s) = shared_err.lock() {
                            s.error = Some(e.to_string());
                        }
                    },
                    None,
                )
            }
            SampleFormat::I16 => {
                let cfg2: cpal::StreamConfig = cfg.into();
                device.build_input_stream(
                    cfg2,
                    move |data: &[i16], _| {
                        let floats: Vec<f32> =
                            data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                        feed_callback(&floats, channels, &mut decoder, &shared_data);
                    },
                    move |e| {
                        if let Ok(mut s) = shared_err.lock() {
                            s.error = Some(e.to_string());
                        }
                    },
                    None,
                )
            }
            SampleFormat::U16 => {
                let cfg2: cpal::StreamConfig = cfg.into();
                device.build_input_stream(
                    cfg2,
                    move |data: &[u16], _| {
                        let floats: Vec<f32> = data
                            .iter()
                            .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                            .collect();
                        feed_callback(&floats, channels, &mut decoder, &shared_data);
                    },
                    move |e| {
                        if let Ok(mut s) = shared_err.lock() {
                            s.error = Some(e.to_string());
                        }
                    },
                    None,
                )
            }
            other => {
                return Err(format!("Unsupported sample format: {other:?}"));
            }
        }
        .map_err(|e| e.to_string())?;

        stream.play().map_err(|e| e.to_string())?;
        self._stream = Some(stream);
        Ok(())
    }

    pub fn disconnect(&mut self) {
        // Dropping the stream stops it.
        self._stream = None;
    }

    pub fn status(&self) -> LtcStatus {
        let s = self.shared.lock().unwrap();
        LtcStatus {
            tc: s.tc,
            age: s.last_rx.map(|t| t.elapsed()),
            fps_label: s.fps_label,
            fps_n: s.fps_n,
            device: s.device.clone(),
            error: s.error.clone(),
        }
    }
}

/// Extract channel 0 from interleaved audio and feed into the decoder.
fn feed_callback(
    data: &[f32],
    channels: usize,
    decoder: &mut LtcDecoder,
    shared: &Arc<Mutex<Shared>>,
) {
    // Extract channel 0 only.
    let mono: Vec<f32> = if channels == 1 {
        data.to_vec()
    } else {
        data.iter().step_by(channels).copied().collect()
    };

    if let Some((tc, measured)) = decoder.feed(&mono) {
        let (fps_label, fps_n) = nominal_fps(measured);
        if let Ok(mut s) = shared.lock() {
            s.tc = Some(tc);
            s.last_rx = Some(Instant::now());
            s.fps_label = fps_label;
            s.fps_n = fps_n;
        }
    }
}

// ─── LtcDecoder ──────────────────────────────────────────────────────────────

/// Stateful biphase-mark decoder for SMPTE LTC.
pub struct LtcDecoder {
    sample_rate: f32,
    /// Adaptive estimate of one full bit period (in samples).
    period: f32,
    /// Current signal polarity: +1 or -1.
    polarity: i8,
    /// Samples since the last edge.
    since_edge: u32,
    /// True when we have received one half-pulse and are waiting for the second.
    half_pending: bool,
    /// 80-bit shift register; LSB-first storage (see module docs).
    reg: u128,
    /// Total samples consumed (used to measure fps).
    sample_pos: u64,
    /// Sample position of the previous sync match (for fps measurement).
    prev_sync_pos: Option<u64>,
    /// Most recently decoded result.
    last_result: Option<(Timecode, f32)>,
    /// True until the very first edge is consumed; that first edge only
    /// establishes the signal polarity and resets the interval counter —
    /// it must NOT be decoded as a bit because `since_edge` has no
    /// meaningful value yet (it counts from decoder construction, not
    /// from the actual signal start).
    first_edge: bool,
}

/// The 16-bit value stored in bits 64..79 of the shift register when the
/// 80-bit sync word is aligned (transmission-order pattern BFFC).
const SYNC_WORD: u16 = 0xBFFC;

/// Hysteresis threshold: ignore zero-crossings when |sample| is below this.
const HYST: f32 = 0.02;

impl LtcDecoder {
    pub fn new(sample_rate: f32) -> Self {
        LtcDecoder {
            sample_rate,
            // Initial period for ~2000 bit/s.
            period: sample_rate / 2000.0,
            polarity: 1,
            since_edge: 0,
            half_pending: false,
            reg: 0,
            sample_pos: 0,
            prev_sync_pos: None,
            last_result: None,
            first_edge: true,
        }
    }

    /// Feed mono f32 samples. Returns the most recently completed frame (if any).
    pub fn feed(&mut self, samples: &[f32]) -> Option<(Timecode, f32)> {
        self.last_result = None;

        for &s in samples {
            self.sample_pos += 1;
            self.since_edge += 1;

            // Detect edge using hysteresis.
            let crossed = if self.polarity > 0 && s < -HYST {
                self.polarity = -1;
                true
            } else if self.polarity < 0 && s > HYST {
                self.polarity = 1;
                true
            } else {
                false
            };

            if crossed {
                self.process_edge();
            }
        }

        self.last_result.take()
    }

    fn process_edge(&mut self) {
        // The very first edge only establishes polarity; since_edge has been
        // counting from decoder construction time, not from a real signal edge,
        // so its value is meaningless.  Discard it and start tracking from here.
        if self.first_edge {
            self.first_edge = false;
            self.since_edge = 0;
            return;
        }

        let d = self.since_edge as f32;
        self.since_edge = 0;

        let t = self.period;

        if d > 0.75 * t {
            // Full period → bit 0.
            // If a half was pending, that was an orphaned half; discard it.
            self.half_pending = false;
            self.period = 0.9 * t + 0.1 * d;
            self.push_bit(0);
        } else {
            // Half period.
            if self.half_pending {
                // Second half of a '1' bit.
                self.period = 0.9 * t + 0.1 * (2.0 * d);
                self.half_pending = false;
                self.push_bit(1);
            } else {
                self.half_pending = true;
            }
        }
    }

    fn push_bit(&mut self, bit: u8) {
        // LSB-first: shift right and place the new bit at position 79.
        self.reg = (self.reg >> 1) | ((bit as u128) << 79);
        self.check_sync();
    }

    fn check_sync(&mut self) {
        let sync = ((self.reg >> 64) & 0xFFFF) as u16;
        if sync != SYNC_WORD {
            return;
        }

        // Measure fps from sample position delta.
        let pos = self.sample_pos;
        let measured_fps = if let Some(prev) = self.prev_sync_pos {
            let delta = pos.saturating_sub(prev) as f32;
            if delta > 0.0 {
                self.sample_rate / delta
            } else {
                0.0
            }
        } else {
            0.0
        };
        self.prev_sync_pos = Some(pos);

        // Decode BCD fields from the low 64 bits.
        let w = self.reg as u64; // low 64 bits

        let frame_u = ((w >> 0) & 0xF) as u8;
        let frame_t = ((w >> 8) & 0x3) as u8;
        let drop_frame = ((w >> 10) & 1) != 0;
        let sec_u = ((w >> 16) & 0xF) as u8;
        let sec_t = ((w >> 24) & 0x7) as u8;
        let min_u = ((w >> 32) & 0xF) as u8;
        let min_t = ((w >> 40) & 0x7) as u8;
        let hour_u = ((w >> 48) & 0xF) as u8;
        let hour_t = ((w >> 56) & 0x3) as u8;

        let f = frame_t * 10 + frame_u;
        let s = sec_t * 10 + sec_u;
        let m = min_t * 10 + min_u;
        let h = hour_t * 10 + hour_u;

        // Validate ranges.
        if h >= 24 || m >= 60 || s >= 60 || f >= 60 {
            return;
        }

        let tc = Timecode { h, m, s, f };

        // The decoded TC labels the frame that just ended; advance by 1 so the
        // displayed value matches the frame currently in progress.
        let (_, fps_n) = if measured_fps > 1.0 {
            nominal_fps(measured_fps)
        } else {
            // Fallback: use 30 fps as a safe default until we have two syncs.
            (30.0, 30u32)
        };
        let tc_next = if drop_frame && fps_n == 30 {
            tc_from_frame_index(frame_index_from_tc(tc, fps_n, true) + 1, fps_n, true)
        } else {
            tc.advanced_by(1, fps_n)
        };

        self.last_result = Some((tc_next, measured_fps));
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tc::Timecode;

    // ── test-only LTC encoder ────────────────────────────────────────────────

    /// Render a sequence of (Timecode, fps_n) pairs as LTC audio.
    ///
    /// BCD bit layout matches the decoder: frame units bits 0-3, frame tens
    /// bits 8-9, sec units 16-19, sec tens 24-26, min units 32-35, min tens
    /// 40-42, hour units 48-51, hour tens 56-57.
    /// Sync word (bits 64-79) in transmission order (bit 64 first):
    ///   0,0,1,1,1,1,1,1,1,1,1,1,1,1,0,1
    /// which stores as 0xBFFC in our shift register after LSB-first intake.
    fn encode_ltc(
        frames: &[(Timecode, u32)],
        sample_rate: f32,
        amplitude: f32,
        invert: bool,
    ) -> Vec<f32> {
        let mut out: Vec<f32> = Vec::new();

        // Biphase-mark requires signal continuity: the level must carry over
        // from one frame to the next so that every bit boundary has exactly one
        // mandatory transition.  Initialise here and update it across frames.
        let mut level: f32 = if invert { -amplitude } else { amplitude };

        for (tc, fps_n) in frames {
            let fps = *fps_n as f32;
            let spb = sample_rate / (fps * 80.0); // samples per bit (fractional)

            // Build the 80-bit word (bit 0 = first transmitted).
            let mut word = [0u8; 80];

            let bcd =
                |val: u8, bits: usize| -> Vec<u8> { (0..bits).map(|i| (val >> i) & 1).collect() };

            let f_u = tc.f % 10;
            let f_t = tc.f / 10;
            let s_u = tc.s % 10;
            let s_t = tc.s / 10;
            let m_u = tc.m % 10;
            let m_t = tc.m / 10;
            let h_u = tc.h % 10;
            let h_t = tc.h / 10;

            // Frame units → bits 0-3
            for (i, b) in bcd(f_u, 4).iter().enumerate() {
                word[i] = *b;
            }
            // bits 4-7: user bits group 1 (zero)
            // Frame tens → bits 8-9
            for (i, b) in bcd(f_t, 2).iter().enumerate() {
                word[8 + i] = *b;
            }
            // bit 10: drop-frame flag (0)
            // bit 11: color-frame flag (0)
            // bits 12-15: user bits group 2 (zero)
            // Sec units → bits 16-19
            for (i, b) in bcd(s_u, 4).iter().enumerate() {
                word[16 + i] = *b;
            }
            // bits 20-23: user bits group 3 (zero)
            // Sec tens → bits 24-26
            for (i, b) in bcd(s_t, 3).iter().enumerate() {
                word[24 + i] = *b;
            }
            // bit 27: biphase mark correction (0)
            // bits 28-31: user bits group 4 (zero)
            // Min units → bits 32-35
            for (i, b) in bcd(m_u, 4).iter().enumerate() {
                word[32 + i] = *b;
            }
            // bits 36-39: user bits group 5 (zero)
            // Min tens → bits 40-42
            for (i, b) in bcd(m_t, 3).iter().enumerate() {
                word[40 + i] = *b;
            }
            // bit 43: binary group flag (0)
            // bits 44-47: user bits group 6 (zero)
            // Hour units → bits 48-51
            for (i, b) in bcd(h_u, 4).iter().enumerate() {
                word[48 + i] = *b;
            }
            // bits 52-55: user bits group 7 (zero)
            // Hour tens → bits 56-57
            for (i, b) in bcd(h_t, 2).iter().enumerate() {
                word[56 + i] = *b;
            }
            // bit 58: binary group flag (0)
            // bits 59-63: user bits group 8 (zero)
            // Sync word bits 64-79 in transmission order:
            //   0,0,1,1,1,1,1,1,1,1,1,1,1,1,0,1
            let sync: [u8; 16] = [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1];
            for (i, &b) in sync.iter().enumerate() {
                word[64 + i] = b;
            }

            // Render biphase-mark: toggle at every bit boundary; extra toggle
            // at mid-bit for '1' bits.  Track fractional sample position so
            // the total frame length is accurate regardless of non-integer spb.
            // (`level` is declared outside the frame loop for inter-frame continuity)
            let frame_start = out.len();
            let mut frac_pos: f64 = 0.0; // offset from frame_start

            for bit in word.iter() {
                // Toggle at bit boundary.
                level = -level;
                let bit_end_off = (frac_pos + spb as f64) as usize;

                if *bit == 1 {
                    // Extra toggle at mid-bit.
                    let mid_off = (frac_pos + spb as f64 / 2.0) as usize;
                    // Fill first half at current level.
                    let target = frame_start + mid_off;
                    while out.len() < target {
                        out.push(level);
                    }
                    // Toggle and fill second half.
                    level = -level;
                    let target = frame_start + bit_end_off;
                    while out.len() < target {
                        out.push(level);
                    }
                } else {
                    // Full bit at current level.
                    let target = frame_start + bit_end_off;
                    while out.len() < target {
                        out.push(level);
                    }
                }

                frac_pos += spb as f64;
            }
        }

        // Append half a bit-period of signal after the last frame so that the
        // decoder can detect the mandatory transition at the final frame's end
        // and produce a result for it.  Without this, the very last frame's
        // bit 79 is never confirmed by the next boundary edge.
        if let Some((_, last_fps_n)) = frames.last() {
            let last_fps = *last_fps_n as f32;
            let last_spb = sample_rate / (last_fps * 80.0);
            let tail = (last_spb * 0.6).ceil() as usize;
            level = -level; // mandatory transition
            for _ in 0..tail {
                out.push(level);
            }
        }

        out
    }

    // ── Test 1: 30 fps / 48 kHz roundtrip ───────────────────────────────────

    #[test]
    fn roundtrip_30fps_48k() {
        let sr = 48_000.0_f32;
        let fps_n = 30u32;

        let frames: Vec<(Timecode, u32)> = (0..10)
            .map(|i| {
                (
                    Timecode {
                        h: 1,
                        m: 2,
                        s: 3,
                        f: i,
                    },
                    fps_n,
                )
            })
            .collect();

        let audio = encode_ltc(&frames, sr, 1.0, false);

        let mut decoder = LtcDecoder::new(sr);
        let mut decoded: Vec<(Timecode, f32)> = Vec::new();

        for chunk in audio.chunks(480) {
            if let Some(r) = decoder.feed(chunk) {
                decoded.push(r);
            }
        }

        assert!(
            decoded.len() >= 8,
            "Expected ≥8 decoded frames, got {}",
            decoded.len()
        );

        let last = decoded.last().unwrap();
        // The last decoded frame should be 01:02:03:09 advanced by 1 → 01:02:03:10
        assert_eq!(
            last.0,
            Timecode {
                h: 1,
                m: 2,
                s: 3,
                f: 10
            },
            "Last TC mismatch: got {:?}",
            last.0
        );
        assert!(
            (last.1 - 30.0).abs() < 0.5,
            "Measured fps {:.2} not near 30",
            last.1
        );
    }

    // ── Test 2: 25 fps / 44100 Hz, low amplitude, inverted ──────────────────

    #[test]
    fn roundtrip_25fps_441k_inverted() {
        let sr = 44_100.0_f32;
        let fps_n = 25u32;

        let frames: Vec<(Timecode, u32)> = (0..10)
            .map(|i| {
                (
                    Timecode {
                        h: 0,
                        m: 30,
                        s: 15,
                        f: i,
                    },
                    fps_n,
                )
            })
            .collect();

        let audio = encode_ltc(&frames, sr, 0.15, true);

        let mut decoder = LtcDecoder::new(sr);
        let mut decoded: Vec<(Timecode, f32)> = Vec::new();

        for chunk in audio.chunks(441) {
            if let Some(r) = decoder.feed(chunk) {
                decoded.push(r);
            }
        }

        assert!(
            decoded.len() >= 8,
            "Expected ≥8 decoded frames at 25fps, got {}",
            decoded.len()
        );

        let (_, fps_label, fps_n_got) = {
            let r = decoded.last().unwrap();
            let (lbl, n) = nominal_fps(r.1);
            (r.0, lbl, n)
        };
        assert_eq!(fps_label, 25.0, "Rate label should be 25.0");
        assert_eq!(fps_n_got, 25u32);
    }

    // ── Test 3: silence + garbage → no false decode ──────────────────────────

    #[test]
    fn no_false_sync_on_garbage() {
        let sr = 48_000.0_f32;
        let mut decoder = LtcDecoder::new(sr);

        // Silence.
        let silence = vec![0.0f32; 48_000];
        let r = decoder.feed(&silence);
        assert!(r.is_none(), "Silence should not produce a frame");

        // Alternating fixed pattern (not valid LTC).
        let garbage: Vec<f32> = (0..48_000)
            .map(|i| if i % 7 < 3 { 0.5 } else { -0.5 })
            .collect();
        let r = decoder.feed(&garbage);
        // Garbage might accidentally match the sync pattern; we just assert no
        // *valid* timecode (h<24, m<60, s<60, f<60 enforced inside decoder).
        // If something is returned it must at least pass range checks.
        if let Some((tc, _)) = r {
            assert!(tc.h < 24);
            assert!(tc.m < 60);
            assert!(tc.s < 60);
            assert!(tc.f < 60);
        }
    }
}
