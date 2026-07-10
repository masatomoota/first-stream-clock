//! MIDI Time Code (MTC) output.
//!
//! A worker thread owns the MIDI connection and emits quarter-frame messages
//! from the lock-free [`TcTarget`] maintained by the UI thread. Stopped targets
//! keep emitting the same latched timecode, as required for a stopped MTC
//! generator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use midir::{MidiOutput, MidiOutputConnection};

use crate::tc::{frame_index_from_seconds, tc_from_frame_index, TcRate, TcTarget, Timecode};

/// The eight quarter-frame data bytes for `tc` (`0xF1` status stripped).
pub(crate) fn quarter_frame_data(tc: Timecode, rate_code: u8) -> [u8; 8] {
    [
        tc.f & 0x0f,
        0x10 | ((tc.f >> 4) & 0x01),
        0x20 | (tc.s & 0x0f),
        0x30 | ((tc.s >> 4) & 0x03),
        0x40 | (tc.m & 0x0f),
        0x50 | ((tc.m >> 4) & 0x03),
        0x60 | (tc.h & 0x0f),
        0x70 | (((rate_code & 0x03) << 1) | ((tc.h >> 4) & 0x01)),
    ]
}

/// A universal real-time MTC full-frame SysEx message for `tc`.
pub(crate) fn full_frame_sysex(tc: Timecode, rate_code: u8) -> [u8; 10] {
    [
        0xf0,
        0x7f,
        0x7f,
        0x01,
        0x01,
        ((rate_code & 0x03) << 5) | (tc.h & 0x1f),
        tc.m,
        tc.s,
        tc.f,
        0xf7,
    ]
}

fn quarter_frame_period_seconds(rate: TcRate) -> f64 {
    let (num, den) = rate.rational();
    den as f64 / (num as f64 * 4.0)
}

/// Number of timecode frames represented by one complete eight-piece cycle.
pub(crate) fn cycle_frames(rate: TcRate) -> f64 {
    let (num, den) = rate.rational();
    quarter_frame_period_seconds(rate) * 8.0 * num as f64 / den as f64
}

fn latch_jumped(
    previous_frame: Option<u64>,
    previous_running: Option<bool>,
    frame_idx: u64,
    running: bool,
    expected_cycle_advance: u64,
) -> bool {
    let Some(previous) = previous_frame else {
        return false;
    };

    if !running && previous_running == Some(false) {
        // A stopped generator intentionally repeats the same frame. Only a
        // real retarget while it remains stopped is a discontinuity.
        frame_idx.abs_diff(previous) > 1
    } else {
        // Running cycles normally advance two frames. Use the same expectation
        // on a running-to-stopped transition so a discontinuous frozen target
        // still receives the required Full-frame message.
        frame_idx.abs_diff(previous.saturating_add(expected_cycle_advance)) > 1
    }
}

#[derive(Default)]
struct Shared {
    port: Option<String>,
    rate: TcRate,
    tc: Option<Timecode>,
    error: Option<String>,
}

/// Snapshot of the MTC sender state for the UI.
#[derive(Clone, Debug)]
pub struct MtcOutStatus {
    // Part of the public status contract; the current UI uses its persisted
    // selection while integrations can use the actually connected port.
    #[allow(dead_code)]
    pub port: Option<String>,
    pub rate_label: &'static str,
    pub tc: Option<Timecode>,
    pub error: Option<String>,
}

/// Owns the MTC output worker and its shared target.
pub struct MtcSender {
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    target: Arc<TcTarget>,
    shared: Arc<Mutex<Shared>>,
}

impl MtcSender {
    pub fn new() -> Self {
        Self {
            worker: None,
            stop: Arc::new(AtomicBool::new(false)),
            target: Arc::new(TcTarget::default()),
            shared: Arc::new(Mutex::new(Shared::default())),
        }
    }

    /// Return all available MIDI output port names.
    pub fn list_ports() -> Vec<String> {
        let midi_out = match MidiOutput::new("StreamClock-list") {
            Ok(output) => output,
            Err(_) => return Vec::new(),
        };

        midi_out
            .ports()
            .iter()
            .filter_map(|port| midi_out.port_name(port).ok())
            .collect()
    }

    /// Start emitting MTC on the exactly named output port.
    ///
    /// The MIDI connection is created on the worker thread. A handshake keeps
    /// this method from returning success until the port is actually open.
    pub fn start(&mut self, port_name: &str, rate: TcRate) -> Result<(), String> {
        self.stop();

        {
            let mut shared = lock_shared(&self.shared);
            shared.port = None;
            shared.rate = rate;
            shared.tc = None;
            shared.error = None;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let target = Arc::clone(&self.target);
        let shared = Arc::clone(&self.shared);
        let requested_port = port_name.to_owned();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);

        let worker = thread::Builder::new()
            .name("stream-clock-mtc-out".to_owned())
            .spawn(move || {
                sender_worker(requested_port, rate, worker_stop, target, shared, ready_tx);
            })
            .map_err(|error| {
                let message = format!("Could not start MTC output thread: {error}");
                lock_shared(&self.shared).error = Some(message.clone());
                message
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.stop = stop;
                self.worker = Some(worker);
                Ok(())
            }
            Ok(Err(error)) => {
                stop.store(true, Ordering::Release);
                let _ = worker.join();
                lock_shared(&self.shared).error = Some(error.clone());
                Err(error)
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = worker.join();
                let message = format!("MTC output worker stopped during startup: {error}");
                lock_shared(&self.shared).error = Some(message.clone());
                Err(message)
            }
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                lock_shared(&self.shared).error =
                    Some("MTC output worker thread panicked".to_owned());
            }
        }
        lock_shared(&self.shared).port = None;
    }

    pub fn is_active(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }

    pub fn target(&self) -> Arc<TcTarget> {
        Arc::clone(&self.target)
    }

    pub fn status(&self) -> MtcOutStatus {
        let shared = lock_shared(&self.shared);
        MtcOutStatus {
            port: shared.port.clone(),
            rate_label: shared.rate.label(),
            tc: shared.tc,
            error: shared.error.clone(),
        }
    }
}

impl Default for MtcSender {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MtcSender {
    fn drop(&mut self) {
        self.stop();
    }
}

fn sender_worker(
    port_name: String,
    rate: TcRate,
    stop: Arc<AtomicBool>,
    target: Arc<TcTarget>,
    shared: Arc<Mutex<Shared>>,
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    let connection = open_port(&port_name);
    let mut connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            lock_shared(&shared).error = Some(error.clone());
            let _ = ready.send(Err(error));
            return;
        }
    };

    {
        let mut state = lock_shared(&shared);
        state.port = Some(port_name);
        state.error = None;
    }
    if ready.send(Ok(())).is_err() {
        lock_shared(&shared).port = None;
        return;
    }

    if let Err(error) = send_loop(&mut connection, &stop, &target, &shared, rate) {
        lock_shared(&shared).error = Some(error);
    }
    lock_shared(&shared).port = None;
}

fn open_port(port_name: &str) -> Result<MidiOutputConnection, String> {
    let midi_out = MidiOutput::new("StreamClock")
        .map_err(|error| format!("Could not initialize MIDI output: {error}"))?;
    let port = midi_out
        .ports()
        .into_iter()
        .find(|port| midi_out.port_name(port).as_deref() == Ok(port_name))
        .ok_or_else(|| format!("MIDI output port not found: {port_name}"))?;

    midi_out
        .connect(&port, "stream-clock-mtc-out")
        .map_err(|error| format!("Could not connect to MIDI output {port_name}: {error}"))
}

fn send_loop(
    connection: &mut MidiOutputConnection,
    stop: &AtomicBool,
    target: &TcTarget,
    shared: &Mutex<Shared>,
    rate: TcRate,
) -> Result<(), String> {
    let period = Duration::from_secs_f64(quarter_frame_period_seconds(rate));
    let (num, den) = rate.rational();
    let rate_code = rate.mtc_rate_code();
    let expected_cycle_advance = cycle_frames(rate).round() as u64;

    let mut next = Instant::now();
    let mut piece = 0usize;
    let mut data = [0u8; 8];
    let mut previous_frame = None;
    let mut previous_running = None;
    let mut initial_full_frame_pending = true;

    while !stop.load(Ordering::Acquire) {
        if !wait_until(next, stop) {
            break;
        }
        if recover_late_deadline(&mut next, period) {
            // Discard a partial pre-suspension cycle. Piece 0 resets receivers'
            // assemblers and relatches the current target below.
            piece = 0;
        }

        if piece == 0 {
            let (Some(seconds), running) = target.now_seconds_and_running() else {
                let _ = advance_deadline(&mut next, period);
                continue;
            };

            let frame_idx = frame_index_from_seconds(seconds, num, den);
            let tc = tc_from_frame_index(frame_idx, rate.fps_n(), rate.drop_frame());
            let resumed = previous_running == Some(false) && running;
            let jumped = latch_jumped(
                previous_frame,
                previous_running,
                frame_idx,
                running,
                expected_cycle_advance,
            );

            if initial_full_frame_pending || jumped || resumed {
                connection
                    .send(&full_frame_sysex(tc, rate_code))
                    .map_err(|error| format!("Could not send MTC full-frame message: {error}"))?;
                initial_full_frame_pending = false;
            }

            data = quarter_frame_data(tc, rate_code);
            previous_frame = Some(frame_idx);
            previous_running = Some(running);
        }

        connection
            .send(&[0xf1, data[piece]])
            .map_err(|error| format!("Could not send MTC quarter-frame message: {error}"))?;

        if piece == 0 {
            let mut state = lock_shared(shared);
            let frame_idx = previous_frame.expect("piece zero always latches a frame");
            state.tc = Some(tc_from_frame_index(
                frame_idx,
                rate.fps_n(),
                rate.drop_frame(),
            ));
            state.error = None;
        }

        piece = (piece + 1) % data.len();
        if advance_deadline(&mut next, period) {
            piece = 0;
        }
    }

    Ok(())
}

fn wait_until(deadline: Instant, stop: &AtomicBool) -> bool {
    const SPIN_WINDOW: Duration = Duration::from_millis(1);

    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }

        let now = Instant::now();
        if now >= deadline {
            return true;
        }

        let remaining = deadline.duration_since(now);
        if remaining > SPIN_WINDOW {
            thread::sleep(remaining - SPIN_WINDOW);
        } else {
            std::hint::spin_loop();
        }
    }
}

fn recover_late_deadline(next: &mut Instant, period: Duration) -> bool {
    let now = Instant::now();
    if now.saturating_duration_since(*next) > period {
        *next = now;
        true
    } else {
        false
    }
}

fn advance_deadline(next: &mut Instant, period: Duration) -> bool {
    *next += period;
    if recover_late_deadline(next, period) {
        // A suspended or overloaded process must not burst stale messages.
        *next += period;
        true
    } else {
        false
    }
}

fn lock_shared(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mtc::{parse_full_frame, QfAssembler};

    const CASES: [Timecode; 3] = [
        Timecode {
            h: 0,
            m: 0,
            s: 0,
            f: 0,
        },
        Timecode {
            h: 23,
            m: 59,
            s: 59,
            f: 29,
        },
        Timecode {
            h: 12,
            m: 34,
            s: 56,
            f: 7,
        },
    ];

    #[test]
    fn quarter_frames_round_trip_through_assembler() {
        for tc in CASES {
            for rate_code in 0..=3 {
                let mut assembler = QfAssembler::new();
                let messages = quarter_frame_data(tc, rate_code);
                for message in &messages[..7] {
                    assert_eq!(assembler.feed(*message), None);
                }
                assert_eq!(assembler.feed(messages[7]), Some((tc, rate_code)));
            }
        }
    }

    #[test]
    fn full_frames_round_trip_through_parser() {
        for tc in CASES {
            for rate_code in 0..=3 {
                let message = full_frame_sysex(tc, rate_code);
                assert_eq!(parse_full_frame(&message), Some((tc, rate_code)));
            }
        }
    }

    #[test]
    fn quarter_frame_piece_indices_are_ordered() {
        let tc = Timecode {
            h: 12,
            m: 34,
            s: 56,
            f: 7,
        };
        for (piece, byte) in quarter_frame_data(tc, 3).into_iter().enumerate() {
            assert_eq!(byte >> 4, piece as u8);
        }
    }

    #[test]
    fn quarter_frame_timing_spans_two_frames_per_cycle() {
        for rate in TcRate::ALL {
            let (num, den) = rate.rational();
            let frame_seconds = den as f64 / num as f64;
            let period = quarter_frame_period_seconds(rate);
            assert!((period * 4.0 - frame_seconds).abs() < 1e-12);
            assert!((cycle_frames(rate) - 2.0).abs() < 1e-12);
        }
    }

    #[test]
    fn latch_jump_detection_distinguishes_running_and_stopped_targets() {
        assert!(!latch_jumped(None, None, 100, true, 2));

        // A running cycle expects +2 frames and tolerates one frame of jitter.
        assert!(!latch_jumped(Some(100), Some(true), 101, true, 2));
        assert!(!latch_jumped(Some(100), Some(true), 102, true, 2));
        assert!(!latch_jumped(Some(100), Some(true), 103, true, 2));
        assert!(latch_jumped(Some(100), Some(true), 104, true, 2));

        // Repeating a frozen frame is normal. A separate frozen retarget is a
        // jump. A running-to-stopped transition is compared with the normal
        // +2-frame expectation, so only a discontinuous stop sends SysEx.
        assert!(!latch_jumped(Some(100), Some(false), 100, false, 2));
        assert!(!latch_jumped(Some(100), Some(false), 101, false, 2));
        assert!(latch_jumped(Some(100), Some(false), 110, false, 2));
        assert!(!latch_jumped(Some(100), Some(true), 102, false, 2));
        assert!(latch_jumped(Some(100), Some(true), 110, false, 2));
    }

    #[test]
    fn missed_deadline_restarts_at_piece_zero_without_a_burst() {
        let period = Duration::from_millis(10);

        let mut overdue = Instant::now() - period.saturating_mul(3);
        assert!(recover_late_deadline(&mut overdue, period));

        let mut piece = 5usize;
        let mut next = Instant::now() - period.saturating_mul(4);
        let stale_deadline = next;
        if advance_deadline(&mut next, period) {
            piece = 0;
        }
        assert_eq!(piece, 0, "a stale partial QF cycle must be discarded");
        assert!(next > stale_deadline, "recovery must advance the deadline");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn mtc_sender_emits_over_real_virtual_midi_port() {
        use crate::tc::frame_index_from_tc;
        use midir::os::unix::VirtualInput;
        use midir::{Ignore, MidiInput};

        let process_id = std::process::id();
        let sink_label = format!("StreamClock MTC Test Sink {process_id}");
        let messages = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let quarter_frame_arrivals = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let callback_messages = Arc::clone(&messages);
        let callback_arrivals = Arc::clone(&quarter_frame_arrivals);

        let mut midi_input = MidiInput::new(&format!("StreamClock MTC Test Input {process_id}"))
            .expect("CoreMIDI/ALSA must initialize for the virtual MTC test");
        midi_input.ignore(Ignore::None);
        let virtual_input = midi_input
            .create_virtual(
                &sink_label,
                move |_timestamp, message, _| {
                    let arrived = Instant::now();
                    callback_messages
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(message.to_vec());
                    if message.len() == 2 && message[0] == 0xf1 {
                        callback_arrivals
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(arrived);
                    }
                },
                (),
            )
            .expect("virtual MTC input port must be created");

        let listed_ports = MtcSender::list_ports();
        let listed_sink = listed_ports
            .iter()
            .find(|name| name.contains(&sink_label))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "virtual MTC sink `{sink_label}` was not discoverable through \
                     MtcSender::list_ports(); available ports: {listed_ports:?}"
                )
            });

        let expected = Timecode {
            h: 10,
            m: 11,
            s: 12,
            f: 13,
        };
        let mut sender = MtcSender::new();
        sender
            .start(&listed_sink, TcRate::F25)
            .unwrap_or_else(|error| panic!("MTC sender must connect to `{listed_sink}`: {error}"));

        let fixed_frame = frame_index_from_tc(expected, 25, false);
        sender
            .target()
            .set((fixed_frame as f64 + 0.5) / 25.0, false);
        thread::sleep(Duration::from_millis(400));

        sender.stop();
        assert!(!sender.is_active(), "MTC worker must stop and join");
        let (_midi_input, ()) = virtual_input.close();

        let messages = messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let quarter_frame_arrivals = quarter_frame_arrivals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert!(
            !messages.is_empty(),
            "the virtual sink received no MTC data"
        );

        let expected_full_frame = [0xf0, 0x7f, 0x7f, 0x01, 0x01, 0x2a, 0x0b, 0x0c, 0x0d, 0xf7];
        assert_eq!(
            messages[0].as_slice(),
            expected_full_frame.as_slice(),
            "the first MIDI message must be the startup full-frame SysEx"
        );
        assert_eq!(parse_full_frame(&messages[0]), Some((expected, 1)));

        let quarter_frames = &messages[1..];
        for (offset, message) in quarter_frames.iter().enumerate() {
            assert_eq!(
                message.len(),
                2,
                "message {} after the SysEx was not a two-byte quarter frame: {message:02x?}",
                offset + 1
            );
            assert_eq!(message[0], 0xf1, "unexpected MIDI status in {message:02x?}");
            assert_eq!(
                message[1] >> 4,
                (offset % 8) as u8,
                "quarter-frame piece gap or reordering at message {offset}"
            );
        }

        let mut assembler = QfAssembler::new();
        let mut completed_cycles = 0usize;
        for message in quarter_frames {
            if let Some((timecode, rate_code)) = assembler.feed(message[1]) {
                assert_eq!(timecode, expected);
                assert_eq!(rate_code, 1);
                completed_cycles += 1;
            }
        }
        assert!(
            completed_cycles >= 2,
            "expected at least two complete quarter-frame cycles, got {completed_cycles}"
        );

        assert_eq!(
            quarter_frame_arrivals.len(),
            quarter_frames.len(),
            "each captured quarter frame must have one wall-clock timestamp"
        );
        assert!(
            quarter_frame_arrivals.len() >= 2,
            "at least two quarter frames are required to measure timing"
        );
        let span = quarter_frame_arrivals
            .last()
            .expect("arrival list is non-empty")
            .duration_since(quarter_frame_arrivals[0]);
        let mean_interval_ms =
            span.as_secs_f64() * 1_000.0 / (quarter_frame_arrivals.len() - 1) as f64;
        println!(
            "MTC virtual-port E2E: mean quarter-frame interval {mean_interval_ms:.3} ms; \
             completed cycles {completed_cycles}"
        );
        assert!(
            (6.0..=14.0).contains(&mean_interval_ms),
            "mean quarter-frame interval {mean_interval_ms:.3} ms was outside 10 ms +/- 4 ms"
        );
    }
}
