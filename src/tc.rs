//! SMPTE timecode representation and output timing shared across the app.

// Input-only helpers are unused in the App Store build, even when timecode
// output is enabled. Keep that feature combination warning-free.
#![cfg_attr(not(feature = "full-sources"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timecode {
    pub h: u8,
    pub m: u8,
    pub s: u8,
    pub f: u8,
}

/// Supported output rates. NTSC rates retain their exact rational clock rate;
/// `fps_n` is only the nominal frame-number counting base.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TcRate {
    F23_976,
    F24,
    F25,
    F29_97Df,
    F29_97,
    #[default]
    F30,
}

impl TcRate {
    pub const ALL: [TcRate; 6] = [
        TcRate::F23_976,
        TcRate::F24,
        TcRate::F25,
        TcRate::F29_97Df,
        TcRate::F29_97,
        TcRate::F30,
    ];

    pub const fn rational(self) -> (u32, u32) {
        match self {
            TcRate::F23_976 => (24_000, 1_001),
            TcRate::F24 => (24, 1),
            TcRate::F25 => (25, 1),
            TcRate::F29_97Df | TcRate::F29_97 => (30_000, 1_001),
            TcRate::F30 => (30, 1),
        }
    }

    pub const fn fps_n(self) -> u32 {
        match self {
            TcRate::F23_976 | TcRate::F24 => 24,
            TcRate::F25 => 25,
            TcRate::F29_97Df | TcRate::F29_97 | TcRate::F30 => 30,
        }
    }

    pub const fn drop_frame(self) -> bool {
        matches!(self, TcRate::F29_97Df)
    }

    pub const fn label(self) -> &'static str {
        match self {
            TcRate::F23_976 => "23.976",
            TcRate::F24 => "24",
            TcRate::F25 => "25",
            TcRate::F29_97Df => "29.97 DF",
            TcRate::F29_97 => "29.97",
            TcRate::F30 => "30",
        }
    }

    pub const fn mtc_rate_code(self) -> u8 {
        match self {
            TcRate::F23_976 | TcRate::F24 => 0,
            TcRate::F25 => 1,
            TcRate::F29_97Df | TcRate::F29_97 => 2,
            TcRate::F30 => 3,
        }
    }
}

impl Timecode {
    /// Advance by `frames` at integer rate `fps_n` frames per second.
    /// Wall-clock drift of NTSC rates (29.97 vs 30) is irrelevant here:
    /// this is only used to freewheel between received frames.
    pub fn advanced_by(self, frames: u64, fps_n: u32) -> Timecode {
        let fps = fps_n.max(1) as u64;
        let total = (self.h as u64) * 3600 * fps
            + (self.m as u64) * 60 * fps
            + (self.s as u64) * fps
            + self.f as u64
            + frames;
        let day = 24 * 3600 * fps;
        let total = total % day;
        Timecode {
            h: (total / (3600 * fps)) as u8,
            m: ((total / (60 * fps)) % 60) as u8,
            s: ((total / fps) % 60) as u8,
            f: (total % fps) as u8,
        }
    }

    pub fn hmsf(&self) -> String {
        format!("{:02}:{:02}:{:02}:{:02}", self.h, self.m, self.s, self.f)
    }

    pub fn hms(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.h, self.m, self.s)
    }
}

/// Round a measured rate to the nearest standard SMPTE rate label.
pub fn nominal_fps(measured: f32) -> (f32, u32) {
    // (display label, integer counting rate)
    const RATES: [(f32, u32); 5] = [
        (23.976, 24),
        (24.0, 24),
        (25.0, 25),
        (29.97, 30),
        (30.0, 30),
    ];
    let mut best = RATES[0];
    let mut best_d = f32::MAX;
    for r in RATES {
        let d = (r.0 - measured).abs();
        if d < best_d {
            best_d = d;
            best = r;
        }
    }
    best
}

/// Convert a zero-based frame position since midnight to a SMPTE label.
///
/// For 29.97 DF, labels `:00` and `:01` are omitted at the start of every
/// minute except each tenth minute. The input wraps at the 24-hour boundary.
pub fn tc_from_frame_index(idx: u64, fps_n: u32, drop_frame: bool) -> Timecode {
    let fps = fps_n.max(1) as u64;
    let nominal_index = if drop_frame && fps_n == 30 {
        const FRAMES_PER_10_MINUTES: u64 = 17_982;
        const FRAMES_PER_DAY: u64 = FRAMES_PER_10_MINUTES * 6 * 24;

        let actual = idx % FRAMES_PER_DAY;
        let ten_minute_blocks = actual / FRAMES_PER_10_MINUTES;
        let within_block = actual % FRAMES_PER_10_MINUTES;
        let dropped_before = 18 * ten_minute_blocks + 2 * (within_block.saturating_sub(2) / 1_798);
        actual + dropped_before
    } else {
        idx % (24 * 60 * 60 * fps)
    };

    Timecode {
        h: (nominal_index / (3_600 * fps)) as u8,
        m: ((nominal_index / (60 * fps)) % 60) as u8,
        s: ((nominal_index / fps) % 60) as u8,
        f: (nominal_index % fps) as u8,
    }
}

/// Convert a SMPTE label to its zero-based frame position since midnight.
/// The caller is expected to provide a valid label for the selected rate.
pub fn frame_index_from_tc(tc: Timecode, fps_n: u32, drop_frame: bool) -> u64 {
    let fps = fps_n.max(1) as u64;
    let nominal =
        tc.h as u64 * 3_600 * fps + tc.m as u64 * 60 * fps + tc.s as u64 * fps + tc.f as u64;

    if drop_frame && fps_n == 30 {
        let total_minutes = tc.h as u64 * 60 + tc.m as u64;
        let dropped = 2 * (total_minutes - total_minutes / 10);
        nominal.saturating_sub(dropped) % (17_982 * 6 * 24)
    } else {
        nominal % (24 * 60 * 60 * fps)
    }
}

/// Convert elapsed seconds to a frame index using an exact rational rate.
pub fn frame_index_from_seconds(sec: f64, num: u32, den: u32) -> u64 {
    if !sec.is_finite() || sec <= 0.0 || den == 0 {
        return 0;
    }
    (sec * num as f64 / den as f64).floor() as u64
}

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Monotonic process epoch. [`TcTarget`] timestamps are nanoseconds since this.
pub fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

fn epoch_elapsed_ns() -> u64 {
    epoch().elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// A time-referenced output target shared with real-time generator threads.
/// All reads are lock-free and allocation-free.
pub struct TcTarget {
    seq: AtomicU64,
    seconds: AtomicU64,
    captured_ns: AtomicU64,
    running: AtomicBool,
    valid: AtomicBool,
}

#[derive(Clone, Copy)]
struct TcTargetSnapshot {
    seconds: f64,
    captured_ns: u64,
    running: bool,
}

impl Default for TcTarget {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            seconds: AtomicU64::new(0.0f64.to_bits()),
            captured_ns: AtomicU64::new(0),
            running: AtomicBool::new(false),
            valid: AtomicBool::new(false),
        }
    }
}

impl TcTarget {
    /// Writers are single-producer: only the UI thread may call `set` or
    /// `clear`. Concurrent writers would violate the seqlock protocol.
    pub fn set(&self, seconds: f64, running: bool) {
        self.seq.fetch_add(1, Ordering::Acquire);
        self.seconds.store(seconds.to_bits(), Ordering::Relaxed);
        self.captured_ns
            .store(epoch_elapsed_ns(), Ordering::Relaxed);
        self.running.store(running, Ordering::Relaxed);
        self.valid.store(true, Ordering::Relaxed);
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// Mark the target unavailable without disturbing its last stored value.
    pub fn clear(&self) {
        self.seq.fetch_add(1, Ordering::Acquire);
        self.valid.store(false, Ordering::Relaxed);
        self.seq.fetch_add(1, Ordering::Release);
    }

    fn snapshot(&self) -> Option<TcTargetSnapshot> {
        const MAX_ATTEMPTS: usize = 4;

        for _ in 0..MAX_ATTEMPTS {
            let before = self.seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            let seconds = f64::from_bits(self.seconds.load(Ordering::Relaxed));
            let captured_ns = self.captured_ns.load(Ordering::Relaxed);
            let running = self.running.load(Ordering::Relaxed);
            let valid = self.valid.load(Ordering::Relaxed);

            if self.seq.load(Ordering::Acquire) == before {
                return valid.then_some(TcTargetSnapshot {
                    seconds,
                    captured_ns,
                    running,
                });
            }

            std::hint::spin_loop();
        }

        None
    }

    /// Read the projected seconds and running flag from one coherent snapshot.
    pub(crate) fn now_seconds_and_running(&self) -> (Option<f64>, bool) {
        let Some(snapshot) = self.snapshot() else {
            return (None, false);
        };

        if !snapshot.running {
            return (Some(snapshot.seconds), false);
        }

        let elapsed_ns = epoch_elapsed_ns().saturating_sub(snapshot.captured_ns);
        (
            Some(snapshot.seconds + elapsed_ns as f64 / 1_000_000_000.0),
            true,
        )
    }

    #[allow(dead_code)]
    pub fn now_seconds(&self) -> Option<f64> {
        self.now_seconds_and_running().0
    }

    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.snapshot().is_some_and(|snapshot| snapshot.running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_wraps_seconds_and_midnight() {
        let tc = Timecode {
            h: 23,
            m: 59,
            s: 59,
            f: 29,
        };
        assert_eq!(
            tc.advanced_by(1, 30),
            Timecode {
                h: 0,
                m: 0,
                s: 0,
                f: 0
            }
        );
        let tc = Timecode {
            h: 1,
            m: 0,
            s: 59,
            f: 25,
        };
        assert_eq!(
            tc.advanced_by(10, 30),
            Timecode {
                h: 1,
                m: 1,
                s: 0,
                f: 5
            }
        );
    }

    #[test]
    fn nominal_rates() {
        assert_eq!(nominal_fps(29.96).1, 30);
        assert_eq!(nominal_fps(25.02).0, 25.0);
        assert_eq!(nominal_fps(24.1).0, 24.0);
    }

    #[test]
    fn tc_rate_properties_are_exact() {
        let expected = [
            ((24_000, 1_001), 24, false, "23.976", 0),
            ((24, 1), 24, false, "24", 0),
            ((25, 1), 25, false, "25", 1),
            ((30_000, 1_001), 30, true, "29.97 DF", 2),
            ((30_000, 1_001), 30, false, "29.97", 2),
            ((30, 1), 30, false, "30", 3),
        ];
        for (rate, expected) in TcRate::ALL.into_iter().zip(expected) {
            assert_eq!(rate.rational(), expected.0);
            assert_eq!(rate.fps_n(), expected.1);
            assert_eq!(rate.drop_frame(), expected.2);
            assert_eq!(rate.label(), expected.3);
            assert_eq!(rate.mtc_rate_code(), expected.4);
        }
    }

    #[test]
    fn non_drop_frame_indices_round_trip_and_wrap() {
        for fps in [24, 25, 30] {
            let day = 24 * 60 * 60 * fps as u64;
            for idx in [0, 1, fps as u64 - 1, day / 2, day - 1, day] {
                let tc = tc_from_frame_index(idx, fps, false);
                assert_eq!(frame_index_from_tc(tc, fps, false), idx % day);
            }
        }
    }

    #[test]
    fn drop_frame_indices_are_exact_inverses() {
        const TEN_MINUTES: u64 = 17_982;
        const DAY: u64 = TEN_MINUTES * 6 * 24;

        for idx in 0..=TEN_MINUTES {
            let tc = tc_from_frame_index(idx, 30, true);
            assert_eq!(
                frame_index_from_tc(tc, 30, true),
                idx % DAY,
                "round trip failed at frame {idx}: {tc:?}"
            );
        }

        let last = tc_from_frame_index(DAY - 1, 30, true);
        assert_eq!(
            last,
            Timecode {
                h: 23,
                m: 59,
                s: 59,
                f: 29
            }
        );
        assert_eq!(
            tc_from_frame_index(DAY, 30, true),
            Timecode {
                h: 0,
                m: 0,
                s: 0,
                f: 0
            }
        );
    }

    #[test]
    fn drop_frame_skips_two_labels_except_each_tenth_minute() {
        let before = frame_index_from_tc(
            Timecode {
                h: 0,
                m: 0,
                s: 59,
                f: 29,
            },
            30,
            true,
        );
        assert_eq!(
            tc_from_frame_index(before + 1, 30, true),
            Timecode {
                h: 0,
                m: 1,
                s: 0,
                f: 2
            }
        );

        let before_tenth = frame_index_from_tc(
            Timecode {
                h: 0,
                m: 9,
                s: 59,
                f: 29,
            },
            30,
            true,
        );
        assert_eq!(
            tc_from_frame_index(before_tenth + 1, 30, true),
            Timecode {
                h: 0,
                m: 10,
                s: 0,
                f: 0
            }
        );
    }

    #[test]
    fn seconds_use_rational_rate_and_floor() {
        assert_eq!(frame_index_from_seconds(1.0, 30_000, 1_001), 29);
        assert_eq!(frame_index_from_seconds(1.002, 30_000, 1_001), 30);
        assert_eq!(frame_index_from_seconds(1.0, 24, 1), 24);
        assert_eq!(frame_index_from_seconds(-1.0, 30, 1), 0);
    }

    #[test]
    fn target_freezes_or_extrapolates_without_a_lock() {
        let target = TcTarget::default();
        assert_eq!(target.now_seconds(), None);

        target.set(12.5, false);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(target.now_seconds(), Some(12.5));
        assert!(!target.is_running());

        target.set(20.0, true);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let live = target.now_seconds().expect("target should be valid");
        assert!(live > 20.0, "unexpected live target {live}");
        assert!(target.is_running());

        target.clear();
        assert_eq!(target.now_seconds(), None);
        assert!(!target.is_running());
    }

    #[test]
    fn target_seqlock_keeps_concurrent_reads_monotonic_and_bounded() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::sync::{Arc, Barrier};
        use std::thread;

        const MIN_WRITTEN: f64 = 1_000.0;
        const WRITE_STEP: f64 = 1_000.0;
        const READS: usize = 200_000;
        const EPSILON: f64 = 0.001;

        let target = Arc::new(TcTarget::default());
        let stop = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let started = Instant::now();
        target.set(MIN_WRITTEN, true);

        let writer_target = Arc::clone(&target);
        let writer_stop = Arc::clone(&stop);
        let writer_progress = Arc::clone(&progress);
        let writer_barrier = Arc::clone(&barrier);
        let writer = thread::spawn(move || {
            writer_barrier.wait();
            let mut count = 0u64;
            while !writer_stop.load(Ordering::Acquire) {
                count += 1;
                writer_target.set(MIN_WRITTEN + count as f64 * WRITE_STEP, true);
                writer_progress.store(count, Ordering::Release);
                if count % 64 == 0 {
                    thread::yield_now();
                }
            }
            count
        });

        barrier.wait();
        while progress.load(Ordering::Acquire) == 0 {
            thread::yield_now();
        }

        let mut observed = Vec::with_capacity(READS);
        let mut last_progress = progress.load(Ordering::Acquire);
        for read_index in 0..READS {
            if let Some(value) = target.now_seconds() {
                observed.push(value);
            }

            if read_index % 256 == 255 {
                while progress.load(Ordering::Acquire) == last_progress {
                    thread::yield_now();
                }
                last_progress = progress.load(Ordering::Acquire);
            }
        }

        stop.store(true, Ordering::Release);
        let writes = writer.join().expect("target writer should not panic");
        let elapsed = started.elapsed().as_secs_f64();
        let max_written = MIN_WRITTEN + writes as f64 * WRITE_STEP;

        assert!(
            observed.len() >= 1_000,
            "expected many consistent snapshots, got {} from {READS} reads",
            observed.len()
        );
        assert!(
            observed.iter().all(|value| value.is_finite()
                && *value >= MIN_WRITTEN
                && *value <= max_written + elapsed),
            "snapshot outside [{MIN_WRITTEN}, {}]",
            max_written + elapsed
        );
        assert!(
            observed.windows(2).all(|pair| pair[1] + EPSILON >= pair[0]),
            "a coherent target snapshot moved backwards"
        );
        println!(
            "TcTarget seqlock: {} consistent reads across {writes} writes in {:.3} ms",
            observed.len(),
            elapsed * 1_000.0
        );
    }
}
