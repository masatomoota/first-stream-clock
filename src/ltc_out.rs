//! SMPTE LTC (Linear Timecode) audio output.
//!
//! The real-time callback is allocation-free. It renders a phase-continuous
//! biphase-mark stream and only applies target corrections between complete
//! 80-bit LTC words.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

use crate::tc::{frame_index_from_seconds, tc_from_frame_index, TcRate, TcTarget, Timecode};

const LTC_BITS_PER_FRAME: usize = 80;
const CALLBACK_MONO_BUFFER: usize = 128;
const SYNC_WORD: [u8; 16] = [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1];
// Prevent accumulated floating-point noise from moving an exactly integral
// transition one sample early. Supported LTC periods stay well outside this
// tolerance when their ideal transition is genuinely fractional.
const SAMPLE_EPSILON: f64 = 1.0e-9;

#[derive(Default)]
struct Shared {
    device: Option<String>,
    rate: TcRate,
    tc: Option<Timecode>,
    error: Option<String>,
}

/// Snapshot of the live LTC output state.
#[derive(Clone)]
pub struct LtcOutStatus {
    #[allow(dead_code)]
    pub device: Option<String>,
    pub rate_label: &'static str,
    pub tc: Option<Timecode>,
    pub error: Option<String>,
}

/// Owns a live CPAL output stream.
///
/// CPAL streams are kept on the UI thread, matching the input receiver's
/// lifecycle. Dropping the stream stops audio output.
pub struct LtcGenerator {
    stream: Option<cpal::Stream>,
    target: Arc<TcTarget>,
    shared: Arc<Mutex<Shared>>,
}

impl LtcGenerator {
    pub fn new() -> Self {
        Self {
            stream: None,
            target: Arc::new(TcTarget::default()),
            shared: Arc::new(Mutex::new(Shared::default())),
        }
    }

    /// Return output device names only. Device enumeration errors produce an
    /// empty list so callers can still offer the default-device choice.
    pub fn list_devices() -> Vec<String> {
        match cpal::default_host().output_devices() {
            Ok(devices) => devices.map(|device| device.to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Start emitting LTC through the exact named output device, or through
    /// the system default output when `device_name` is `None`.
    pub fn start(
        &mut self,
        device_name: Option<&str>,
        rate: TcRate,
        level_db: f32,
    ) -> Result<(), String> {
        self.stop();

        {
            let mut shared = self.shared.lock().unwrap();
            shared.device = device_name.map(str::to_owned);
            shared.rate = rate;
            shared.tc = None;
            shared.error = None;
        }

        let result = self.build_stream(device_name, rate, level_db);
        match result {
            Ok(stream) => {
                self.stream = Some(stream);
                Ok(())
            }
            Err(error) => {
                if let Ok(mut shared) = self.shared.lock() {
                    shared.error = Some(error.clone());
                }
                Err(error)
            }
        }
    }

    fn build_stream(
        &self,
        device_name: Option<&str>,
        rate: TcRate,
        level_db: f32,
    ) -> Result<cpal::Stream, String> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(name) => host
                .output_devices()
                .map_err(|error| error.to_string())?
                .find(|device| device.to_string() == name)
                .ok_or_else(|| format!("Output device not found: {name}"))?,
            None => host
                .default_output_device()
                .ok_or_else(|| "No default output device".to_owned())?,
        };

        let actual_device_name = device.to_string();
        let supported_config = device
            .default_output_config()
            .map_err(|error| error.to_string())?;
        let sample_format = supported_config.sample_format();
        let channels = supported_config.channels() as usize;
        let sample_rate = supported_config.sample_rate() as f64;

        if channels == 0 {
            return Err("Output device reported zero channels".to_owned());
        }

        {
            let mut shared = self.shared.lock().unwrap();
            shared.device = Some(actual_device_name);
        }

        let config: cpal::StreamConfig = supported_config.into();
        let target = Arc::clone(&self.target);
        let shared_data = Arc::clone(&self.shared);
        let shared_error = Arc::clone(&self.shared);
        let mut core = LtcCore::new(sample_rate, rate, level_db);

        let stream = match sample_format {
            SampleFormat::F32 => device.build_output_stream(
                config,
                move |data: &mut [f32], _| {
                    render_f32(data, channels, &mut core, rate, &target, &shared_data);
                },
                move |error| publish_stream_error(&shared_error, error.to_string()),
                None,
            ),
            SampleFormat::I16 => device.build_output_stream(
                config,
                move |data: &mut [i16], _| {
                    render_i16(data, channels, &mut core, rate, &target, &shared_data);
                },
                move |error| publish_stream_error(&shared_error, error.to_string()),
                None,
            ),
            other => return Err(format!("Unsupported output sample format: {other:?}")),
        }
        .map_err(|error| error.to_string())?;

        stream.play().map_err(|error| error.to_string())?;
        Ok(stream)
    }

    pub fn stop(&mut self) {
        self.stream = None;
    }

    /// true while a stream object is held; check `status().error` to know whether it is actually producing audio.
    pub fn is_active(&self) -> bool {
        self.stream.is_some()
    }

    pub fn target(&self) -> Arc<TcTarget> {
        Arc::clone(&self.target)
    }

    pub fn status(&self) -> LtcOutStatus {
        let shared = self.shared.lock().unwrap();
        LtcOutStatus {
            device: shared.device.clone(),
            rate_label: shared.rate.label(),
            tc: shared.tc,
            error: shared.error.clone(),
        }
    }
}

impl Default for LtcGenerator {
    fn default() -> Self {
        Self::new()
    }
}

fn callback_target(target: &TcTarget) -> (Option<f64>, bool) {
    target.now_seconds_and_running()
}

fn projected_target_frame(
    target_seconds: Option<f64>,
    running: bool,
    sample_offset: usize,
    sample_rate: f64,
    rate: TcRate,
) -> Option<u64> {
    let (num, den) = rate.rational();
    target_seconds.map(|seconds| {
        let projected = if running {
            seconds + sample_offset as f64 / sample_rate
        } else {
            seconds
        };
        frame_index_from_seconds(projected, num, den)
    })
}

fn render_f32(
    data: &mut [f32],
    channels: usize,
    core: &mut LtcCore,
    rate: TcRate,
    target: &TcTarget,
    shared: &Arc<Mutex<Shared>>,
) {
    let (target_seconds, running) = callback_target(target);
    let mut mono = [0.0f32; CALLBACK_MONO_BUFFER];
    let mut sample_offset = 0usize;

    for output_chunk in data.chunks_mut(channels * CALLBACK_MONO_BUFFER) {
        let frame_count = output_chunk.len().div_ceil(channels);
        let target_frame = projected_target_frame(
            target_seconds,
            running,
            sample_offset,
            core.sample_rate,
            rate,
        );
        core.fill_target(&mut mono[..frame_count], target_frame, running);
        for (frame, sample) in output_chunk.chunks_mut(channels).zip(mono.iter()) {
            frame.fill(*sample);
        }
        sample_offset += frame_count;
    }

    publish_timecode(shared, core.current_tc());
}

fn render_i16(
    data: &mut [i16],
    channels: usize,
    core: &mut LtcCore,
    rate: TcRate,
    target: &TcTarget,
    shared: &Arc<Mutex<Shared>>,
) {
    let (target_seconds, running) = callback_target(target);
    let mut mono = [0.0f32; CALLBACK_MONO_BUFFER];
    let mut sample_offset = 0usize;

    for output_chunk in data.chunks_mut(channels * CALLBACK_MONO_BUFFER) {
        let frame_count = output_chunk.len().div_ceil(channels);
        let target_frame = projected_target_frame(
            target_seconds,
            running,
            sample_offset,
            core.sample_rate,
            rate,
        );
        core.fill_target(&mut mono[..frame_count], target_frame, running);
        for (frame, sample) in output_chunk.chunks_mut(channels).zip(mono.iter()) {
            let converted = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            frame.fill(converted);
        }
        sample_offset += frame_count;
    }

    publish_timecode(shared, core.current_tc());
}

fn publish_timecode(shared: &Arc<Mutex<Shared>>, tc: Timecode) {
    if let Ok(mut shared) = shared.try_lock() {
        shared.tc = Some(tc);
    }
}

fn publish_stream_error(shared: &Arc<Mutex<Shared>>, error: String) {
    if let Ok(mut shared) = shared.lock() {
        shared.error = Some(error);
    }
}

/// Pure LTC waveform generator driven by tests and by the CPAL callback.
pub(crate) struct LtcCore {
    sample_rate: f64,
    rate: TcRate,
    amplitude: f32,
    level: f32,
    frame_idx: u64,
    bit: usize,
    bit_pos: f64,
    word: [u8; LTC_BITS_PER_FRAME],
    mid_done: bool,
    started: bool,
}

impl LtcCore {
    pub(crate) fn new(sample_rate: f64, rate: TcRate, level_db: f32) -> Self {
        let amplitude = 10.0f32.powf(level_db / 20.0).clamp(0.0, 1.0);
        let frame_idx = 0;
        let word = frame_bits(
            tc_from_frame_index(frame_idx, rate.fps_n(), rate.drop_frame()),
            rate.drop_frame(),
        );

        Self {
            sample_rate,
            rate,
            amplitude,
            level: amplitude,
            frame_idx,
            bit: 0,
            bit_pos: 0.0,
            word,
            mid_done: false,
            started: false,
        }
    }

    /// Emit mono samples. `None` free-runs; a target is reconciled only at an
    /// LTC frame boundary, so an in-progress word is never torn.
    #[allow(dead_code)] // Required pure/core API; production uses hold-aware fill_target.
    pub(crate) fn fill(&mut self, out: &mut [f32], target_frame: Option<u64>) {
        self.fill_target(out, target_frame, true);
    }

    /// Running-aware variant for the live callback. A stopped target repeats
    /// its frozen frame label, which is valid LTC generator behaviour.
    fn fill_target(&mut self, out: &mut [f32], target_frame: Option<u64>, running: bool) {
        let samples_per_bit = self.samples_per_bit();
        for sample in out {
            *sample = self.next_sample(samples_per_bit, target_frame, running);
        }
    }

    pub(crate) fn current_tc(&self) -> Timecode {
        tc_from_frame_index(self.frame_idx, self.rate.fps_n(), self.rate.drop_frame())
    }

    fn samples_per_bit(&self) -> f64 {
        let (num, den) = self.rate.rational();
        self.sample_rate * den as f64 / (num as f64 * LTC_BITS_PER_FRAME as f64)
    }

    fn next_sample(
        &mut self,
        samples_per_bit: f64,
        target_frame: Option<u64>,
        running: bool,
    ) -> f32 {
        if !self.started {
            if let Some(target) = target_frame {
                self.set_frame(target);
            }
            self.level = -self.level;
            self.started = true;
        }

        // Transitions are quantized to the sample immediately preceding their
        // ideal fractional time. Carrying the residual in `bit_pos` distributes
        // those quantization errors across the frame instead of rounding every
        // bit independently.
        if self.bit_pos + 1.0 > samples_per_bit + SAMPLE_EPSILON {
            self.bit_pos -= samples_per_bit;
            self.advance_bit(target_frame, running);
            self.level = -self.level;
        }

        if self.word[self.bit] == 1
            && !self.mid_done
            && self.bit_pos + 1.0 > samples_per_bit / 2.0 + SAMPLE_EPSILON
        {
            self.level = -self.level;
            self.mid_done = true;
        }

        let sample = self.level.clamp(-self.amplitude, self.amplitude);
        self.bit_pos += 1.0;
        sample
    }

    fn advance_bit(&mut self, target_frame: Option<u64>, running: bool) {
        if self.bit + 1 < LTC_BITS_PER_FRAME {
            self.bit += 1;
            self.mid_done = false;
            return;
        }

        self.bit = 0;
        self.mid_done = false;

        let sequential = self.frame_idx.wrapping_add(1);
        let next = match target_frame {
            Some(target) if !running => target,
            Some(target) if target.abs_diff(sequential) >= 2 => target,
            _ => sequential,
        };
        self.set_frame(next);
    }

    fn set_frame(&mut self, frame_idx: u64) {
        self.frame_idx = frame_idx;
        self.word = frame_bits(self.current_tc(), self.rate.drop_frame());
    }
}

fn frame_bits(tc: Timecode, drop_frame: bool) -> [u8; LTC_BITS_PER_FRAME] {
    let mut word = [0u8; LTC_BITS_PER_FRAME];

    write_lsb(&mut word, 0, 4, tc.f % 10);
    write_lsb(&mut word, 8, 2, tc.f / 10);
    word[10] = u8::from(drop_frame);
    write_lsb(&mut word, 16, 4, tc.s % 10);
    write_lsb(&mut word, 24, 3, tc.s / 10);
    write_lsb(&mut word, 32, 4, tc.m % 10);
    write_lsb(&mut word, 40, 3, tc.m / 10);
    write_lsb(&mut word, 48, 4, tc.h % 10);
    write_lsb(&mut word, 56, 2, tc.h / 10);
    word[64..80].copy_from_slice(&SYNC_WORD);

    // Bit 27 is the biphase-mark correction bit. It makes the total number of
    // one bits even, preserving the polarity seen at each frame boundary.
    let ones_without_correction: u32 = word
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 27)
        .map(|(_, bit)| *bit as u32)
        .sum();
    word[27] = (ones_without_correction % 2) as u8;

    word
}

fn write_lsb(word: &mut [u8; LTC_BITS_PER_FRAME], start: usize, width: usize, value: u8) {
    for offset in 0..width {
        word[start + offset] = (value >> offset) & 1;
    }
}

#[cfg(all(test, feature = "full-sources"))]
mod tests {
    use super::*;
    use crate::ltc::LtcDecoder;
    use crate::tc::frame_index_from_tc;

    fn render_frames(
        sample_rate: u64,
        rate: TcRate,
        start: u64,
        frame_count: u64,
    ) -> (LtcCore, Vec<f32>) {
        let mut core = LtcCore::new(sample_rate as f64, rate, -6.0);
        let (num, den) = rate.rational();
        let mut audio = Vec::new();
        let mut previous_end = 0u64;

        // Use cumulative rational boundaries; no frame or bit duration is
        // independently rounded.
        for offset in 0..frame_count {
            let end = (offset + 1) * sample_rate * den as u64 / num as u64;
            let mut frame = vec![0.0; (end - previous_end) as usize];
            core.fill(&mut frame, Some(start + offset));
            audio.extend(frame);
            previous_end = end;
        }

        (core, audio)
    }

    fn decode(sample_rate: f32, audio: &[f32]) -> Vec<Timecode> {
        let mut decoder = LtcDecoder::new(sample_rate);
        let mut decoded = Vec::new();
        for chunk in audio.chunks(97) {
            if let Some((tc, _)) = decoder.feed(chunk) {
                decoded.push(tc);
            }
        }
        decoded
    }

    fn assert_consecutive(decoded: &[Timecode], start: u64, rate: TcRate) {
        assert!(
            decoded.len() >= 5,
            "expected at least five decoded frames, got {decoded:?}"
        );

        let indices: Vec<u64> = decoded
            .iter()
            .map(|tc| frame_index_from_tc(*tc, rate.fps_n(), rate.drop_frame()))
            .collect();
        assert!(
            indices[0] == start || indices[0] == start + 1,
            "unexpected decoder lead-in: {decoded:?}"
        );
        for pair in indices.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "non-consecutive decode: {decoded:?}");
        }
    }

    #[test]
    fn round_trip_30_fps_at_48_khz() {
        let rate = TcRate::F30;
        let start = frame_index_from_tc(
            Timecode {
                h: 1,
                m: 2,
                s: 3,
                f: 4,
            },
            rate.fps_n(),
            false,
        );
        let (_, audio) = render_frames(48_000, rate, start, 14);
        let decoded = decode(48_000.0, &audio);
        assert_consecutive(&decoded, start, rate);
    }

    #[test]
    fn round_trip_25_fps_at_44_1_khz() {
        let rate = TcRate::F25;
        let start = frame_index_from_tc(
            Timecode {
                h: 1,
                m: 2,
                s: 3,
                f: 4,
            },
            rate.fps_n(),
            false,
        );
        let (_, audio) = render_frames(44_100, rate, start, 14);
        let decoded = decode(44_100.0, &audio);
        assert_consecutive(&decoded, start, rate);
    }

    #[test]
    fn round_trip_29_97_df_skips_dropped_labels() {
        let rate = TcRate::F29_97Df;
        let start_tc = Timecode {
            h: 0,
            m: 0,
            s: 59,
            f: 26,
        };
        let start = frame_index_from_tc(start_tc, rate.fps_n(), true);
        let (core, audio) = render_frames(48_000, rate, start, 10);

        // The pure generator crosses 00:00:59:29 directly to 00:01:00:02.
        assert_eq!(
            tc_from_frame_index(start + 3, rate.fps_n(), true),
            Timecode {
                h: 0,
                m: 0,
                s: 59,
                f: 29
            }
        );
        assert_eq!(
            tc_from_frame_index(start + 4, rate.fps_n(), true),
            Timecode {
                h: 0,
                m: 1,
                s: 0,
                f: 2
            }
        );
        assert_eq!(core.current_tc(), tc_from_frame_index(start + 9, 30, true));

        let decoded = decode(48_000.0, &audio);
        assert!(
            decoded.windows(2).any(|pair| {
                pair[0]
                    == Timecode {
                        h: 0,
                        m: 0,
                        s: 59,
                        f: 29,
                    }
                    && pair[1]
                        == Timecode {
                            h: 0,
                            m: 1,
                            s: 0,
                            f: 2,
                        }
            }),
            "decoded sequence did not cross the DF boundary: {decoded:?}"
        );
        assert!(!decoded.iter().any(|tc| tc.m == 1 && tc.s == 0 && tc.f < 2));
    }

    #[test]
    fn target_jump_snaps_only_at_frame_boundary() {
        let rate = TcRate::F30;
        let start = 100u64;
        let target = start + 10;
        let mut core = LtcCore::new(48_000.0, rate, -6.0);
        let mut sample = [0.0];
        let mut audio = Vec::new();

        core.fill(&mut sample, Some(start));
        audio.push(sample[0]);
        for _ in 0..700 {
            core.fill(&mut sample, Some(target));
            audio.push(sample[0]);
            assert_eq!(core.frame_idx, start, "snapped before completing a frame");
        }

        let mut snapped = false;
        for _ in 0..1_000 {
            core.fill(&mut sample, Some(target));
            audio.push(sample[0]);
            if core.frame_idx == target {
                assert_eq!(core.bit, 0, "target snap emitted a partial LTC word");
                snapped = true;
                break;
            }
        }
        assert!(snapped, "target was not applied at the next frame boundary");

        // Continue with a wall-clock-like advancing target. The decoder must
        // recover on the first complete post-jump word and stay consecutive.
        let samples_per_frame = 48_000 / 30;
        for offset in 1..samples_per_frame * 7 {
            let projected_target = target + (offset / samples_per_frame) as u64;
            core.fill(&mut sample, Some(projected_target));
            audio.push(sample[0]);
        }
        let decoded = decode(48_000.0, &audio);
        let post_jump: Vec<u64> = decoded
            .iter()
            .map(|tc| frame_index_from_tc(*tc, 30, false))
            .filter(|idx| *idx >= target)
            .collect();
        assert!(
            post_jump.len() >= 5,
            "decoder did not resynchronize after jump: {decoded:?}"
        );
        assert_eq!(post_jump[0], target + 1);
        for pair in post_jump.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "post-jump decode regressed");
        }
    }

    #[test]
    fn large_callback_projects_running_target_without_backward_snap() {
        let rate = TcRate::F30;
        let start = 100u64;
        let target_seconds = Some((start as f64 + 0.25) / 30.0);
        let mut core = LtcCore::new(48_000.0, rate, -6.0);
        let mut mono = [0.0f32; CALLBACK_MONO_BUFFER];
        let mut sample_offset = 0usize;
        let mut previous_frame = start;

        while sample_offset < 4_096 {
            let count = CALLBACK_MONO_BUFFER.min(4_096 - sample_offset);
            let target_frame =
                projected_target_frame(target_seconds, true, sample_offset, core.sample_rate, rate);
            core.fill_target(&mut mono[..count], target_frame, true);
            assert!(
                core.frame_idx >= previous_frame,
                "large callback snapped backward from {previous_frame} to {}",
                core.frame_idx
            );
            previous_frame = core.frame_idx;
            sample_offset += count;
        }
        assert!(core.frame_idx >= start + 2);
    }

    #[test]
    fn frame_bits_have_sync_word_and_even_parity() {
        let cases = [
            (
                Timecode {
                    h: 0,
                    m: 0,
                    s: 0,
                    f: 0,
                },
                false,
            ),
            (
                Timecode {
                    h: 12,
                    m: 34,
                    s: 56,
                    f: 7,
                },
                false,
            ),
            (
                Timecode {
                    h: 23,
                    m: 59,
                    s: 59,
                    f: 29,
                },
                true,
            ),
        ];

        for (tc, drop_frame) in cases {
            let word = frame_bits(tc, drop_frame);
            assert_eq!(word[64..80], SYNC_WORD);
            assert_eq!(word.iter().map(|bit| *bit as u32).sum::<u32>() % 2, 0);
            assert_eq!(word[10], u8::from(drop_frame));
        }
    }

    #[test]
    fn stopped_target_repeats_the_frozen_frame() {
        let mut core = LtcCore::new(48_000.0, TcRate::F30, -6.0);
        let frozen = 1234;
        let mut audio = vec![0.0; 1_601];
        core.fill_target(&mut audio, Some(frozen), false);
        assert_eq!(core.frame_idx, frozen);
        assert_eq!(core.bit, 0);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod coreaudio_tests {
    use super::*;
    use crate::tc::frame_index_from_tc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn real_coreaudio_output_stream_advances_running_timecode() {
        if LtcGenerator::list_devices().is_empty() {
            println!("skipping CoreAudio LTC output test: no output devices available");
            return;
        }

        let rate = TcRate::F30;
        let start_tc = Timecode {
            h: 10,
            m: 11,
            s: 12,
            f: 13,
        };
        let start_index = frame_index_from_tc(start_tc, rate.fps_n(), rate.drop_frame());
        let start_seconds = start_index as f64 / rate.fps_n() as f64;

        let mut generator = LtcGenerator::new();
        generator
            .start(None, rate, -90.0)
            .expect("default CoreAudio output stream should start");

        let started_at = Instant::now();
        generator.target().set(start_seconds, true);
        thread::sleep(Duration::from_millis(400));

        let elapsed = started_at.elapsed();
        let status = generator.status();
        let active_before_stop = generator.is_active();
        generator.stop();
        let active_after_stop = generator.is_active();

        assert!(
            status.error.is_none(),
            "CoreAudio output reported an error: {:?}",
            status.error
        );
        let end_tc = status
            .tc
            .expect("CoreAudio callback should publish a timecode");
        assert!(
            active_before_stop,
            "generator should hold its CoreAudio stream before stop"
        );
        assert!(
            !active_after_stop,
            "generator should release its CoreAudio stream after stop"
        );

        let end_index = frame_index_from_tc(end_tc, rate.fps_n(), rate.drop_frame());
        let frames_per_day = 24_u64 * 60 * 60 * u64::from(rate.fps_n());
        let frame_advance = (end_index + frames_per_day - start_index) % frames_per_day;
        let (rate_num, rate_den) = rate.rational();
        let expected_advance =
            (elapsed.as_secs_f64() * rate_num as f64 / rate_den as f64).round() as u64;

        println!(
            "CoreAudio LTC output: elapsed={:.3} ms, frame advance={frame_advance}, expected={expected_advance}",
            elapsed.as_secs_f64() * 1_000.0
        );
        assert!(
            frame_advance.abs_diff(expected_advance) <= 4,
            "CoreAudio LTC advanced {frame_advance} frames in {:.3} ms; expected {expected_advance} ± 4",
            elapsed.as_secs_f64() * 1_000.0
        );
    }
}
