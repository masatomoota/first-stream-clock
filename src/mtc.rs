//! MIDI Time Code (MTC) receiver.
//! Uses `midir` to listen on a MIDI input port and decodes both
//! quarter-frame sequences and full-frame SysEx messages into
//! a [`Timecode`] value that the UI can poll via [`MtcReceiver::status`].

#![cfg_attr(not(feature = "full-sources"), allow(dead_code))]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use midir::{Ignore, MidiInput, MidiInputConnection};

use crate::tc::Timecode;

// ---------------------------------------------------------------------------
// Rate-code helpers
// ---------------------------------------------------------------------------

/// Map an MTC rate code (0-3) to `(fps_label, fps_n)`.
fn rate_code_to_fps(code: u8) -> (f32, u32) {
    match code & 0x03 {
        0 => (24.0, 24),
        1 => (25.0, 25),
        2 => (29.97, 30),
        _ => (30.0, 30),
    }
}

// ---------------------------------------------------------------------------
// Quarter-frame assembler (pure, no MIDI dependency)
// ---------------------------------------------------------------------------

/// Assembles MIDI quarter-frame nibbles into a complete [`Timecode`].
///
/// Pieces arrive as 8 separate messages over 2 frames.  We track which
/// pieces we have seen with a bitmask, reset on piece 0, and emit a result
/// when all 8 have arrived.
#[derive(Default)]
pub struct QfAssembler {
    nibbles: [u8; 8], // nibble[i] = value for piece i
    seen: u8,         // bit i set when nibble[i] has been received
}

impl QfAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one quarter-frame data byte (`0xF1` status already stripped).
    /// Returns `Some((tc, rate_code))` when a complete sequence 0-7 has been
    /// received, where `tc` is the raw assembled value *before* the +2 frame
    /// correction (the caller applies that correction if desired).
    pub fn feed(&mut self, data_byte: u8) -> Option<(Timecode, u8)> {
        let piece = (data_byte >> 4) & 0x07;
        let nibble = data_byte & 0x0F;

        // Piece 0 marks the start of a new sequence; reset state.
        if piece == 0 {
            self.seen = 0;
            self.nibbles = [0u8; 8];
        }

        self.nibbles[piece as usize] = nibble;
        self.seen |= 1 << piece;

        // Emit only when all 8 pieces have been seen.
        if self.seen != 0xFF {
            return None;
        }

        let frame = self.nibbles[0] | ((self.nibbles[1] & 0x01) << 4);
        let sec = self.nibbles[2] | ((self.nibbles[3] & 0x03) << 4);
        let min = self.nibbles[4] | ((self.nibbles[5] & 0x03) << 4);
        // Piece 7 nibble: bit0 = hour bit4, bits1-2 = rate code
        let hour = self.nibbles[6] | ((self.nibbles[7] & 0x01) << 4);
        let rate_code = (self.nibbles[7] >> 1) & 0x03;

        let tc = Timecode {
            h: hour,
            m: min,
            s: sec,
            f: frame,
        };
        Some((tc, rate_code))
    }
}

// ---------------------------------------------------------------------------
// Full-frame SysEx parser (pure helper)
// ---------------------------------------------------------------------------

/// Parse an MTC full-frame SysEx message.
///
/// Format: `F0 7F 7F 01 01 hh mm ss ff F7`
/// where `hh` bits 5-6 = rate code, bits 0-4 = hour.
///
/// Returns `Some((tc, rate_code))` or `None` if the message is not a valid
/// MTC full-frame SysEx.
pub fn parse_full_frame(msg: &[u8]) -> Option<(Timecode, u8)> {
    if msg.len() < 10 {
        return None;
    }
    // Validate fixed header and terminator.
    if msg[0] != 0xF0
        || msg[1] != 0x7F
        || msg[2] != 0x7F
        || msg[3] != 0x01
        || msg[4] != 0x01
        || msg[9] != 0xF7
    {
        return None;
    }
    let hh = msg[5];
    let rate_code = (hh >> 5) & 0x03;
    let hour = hh & 0x1F;
    let tc = Timecode {
        h: hour,
        m: msg[6],
        s: msg[7],
        f: msg[8],
    };
    Some((tc, rate_code))
}

// ---------------------------------------------------------------------------
// Shared state written by the MIDI callback
// ---------------------------------------------------------------------------

struct Inner {
    tc: Option<Timecode>,
    last_update: Option<Instant>,
    fps_label: f32,
    fps_n: u32,
    error: Option<String>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            tc: None,
            last_update: None,
            fps_label: 30.0,
            fps_n: 30,
            error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Public status snapshot
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MtcStatus {
    /// Last assembled timecode (after the +2 frame correction for QF messages).
    pub tc: Option<Timecode>,
    /// Time elapsed since the last timecode update.
    pub age: Option<std::time::Duration>,
    /// Display fps label: 24.0 / 25.0 / 29.97 / 30.0.
    pub fps_label: f32,
    /// Integer counting rate: 24 / 25 / 30.
    pub fps_n: u32,
    /// Name of the currently connected MIDI input port.
    pub port: Option<String>,
    /// Last connection error, if any.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// MTC receiver
// ---------------------------------------------------------------------------

pub struct MtcReceiver {
    /// Live MIDI connection — dropping this closes the port.
    connection: Option<MidiInputConnection<()>>,
    /// Name of the currently connected port.
    port_name: Option<String>,
    /// State shared with the MIDI callback.
    inner: Arc<Mutex<Inner>>,
}

impl MtcReceiver {
    /// Create an unconnected receiver.
    pub fn new() -> Self {
        Self {
            connection: None,
            port_name: None,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Return the names of all available MIDI input ports.
    /// Returns an empty `Vec` on any error so callers never need to handle `Err`.
    pub fn list_ports() -> Vec<String> {
        let midi_in = match MidiInput::new("stream-clock-list") {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        let ports = midi_in.ports();
        let mut names = Vec::with_capacity(ports.len());
        for p in &ports {
            if let Ok(name) = midi_in.port_name(p) {
                names.push(name);
            }
        }
        names
    }

    /// Connect to the MIDI input port whose name equals `port_name`.
    /// Any existing connection is dropped first.
    pub fn connect(&mut self, port_name: &str) -> Result<(), String> {
        // Always disconnect first so the old callback stops writing.
        self.disconnect();

        let midi_in = MidiInput::new("stream-clock-mtc").map_err(|e| e.to_string())?;

        // MTC uses 0xF1 (quarter-frame) and 0xF0 (SysEx) — both are filtered
        // by midir by default.  Disable all filtering.
        let mut midi_in = midi_in;
        midi_in.ignore(Ignore::None);

        let ports = midi_in.ports();
        let port = ports
            .iter()
            .find(|p| midi_in.port_name(p).as_deref() == Ok(port_name))
            .ok_or_else(|| format!("MIDI port not found: {port_name}"))?
            .clone();

        let inner = Arc::clone(&self.inner);

        // Reset shared state for the new connection.
        if let Ok(mut g) = inner.lock() {
            *g = Inner::default();
        }

        let mut assembler = QfAssembler::new();

        let conn = midi_in
            .connect(
                &port,
                "mtc-recv",
                move |_stamp, msg, _| {
                    // Quarter-frame: single 2-byte message [0xF1, data]
                    if msg.len() == 2 && msg[0] == 0xF1 {
                        if let Some((raw_tc, rate_code)) = assembler.feed(msg[1]) {
                            let (fps_label, fps_n) = rate_code_to_fps(rate_code);
                            // The assembled TC corresponds to the moment piece 0
                            // was sent; 8 quarter-frames span exactly 2 frames.
                            let tc = raw_tc.advanced_by(2, fps_n);
                            if let Ok(mut g) = inner.lock() {
                                g.tc = Some(tc);
                                g.last_update = Some(Instant::now());
                                g.fps_label = fps_label;
                                g.fps_n = fps_n;
                                g.error = None;
                            }
                        }
                        return;
                    }

                    // Full-frame SysEx: F0 7F 7F 01 01 hh mm ss ff F7
                    if msg.first() == Some(&0xF0) {
                        if let Some((tc, rate_code)) = parse_full_frame(msg) {
                            let (fps_label, fps_n) = rate_code_to_fps(rate_code);
                            if let Ok(mut g) = inner.lock() {
                                g.tc = Some(tc);
                                g.last_update = Some(Instant::now());
                                g.fps_label = fps_label;
                                g.fps_n = fps_n;
                                g.error = None;
                            }
                        }
                    }
                },
                (),
            )
            .map_err(|e| e.to_string())?;

        self.connection = Some(conn);
        self.port_name = Some(port_name.to_string());
        Ok(())
    }

    /// Drop the active connection (if any).
    pub fn disconnect(&mut self) {
        self.connection = None;
        self.port_name = None;
    }

    /// Return a snapshot of the current MTC state.
    pub fn status(&self) -> MtcStatus {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        MtcStatus {
            tc: g.tc,
            age: g.last_update.map(|t| t.elapsed()),
            fps_label: g.fps_label,
            fps_n: g.fps_n,
            port: self.port_name.clone(),
            error: g.error.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests (no MIDI hardware required — only pure logic is tested)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the 8 quarter-frame data bytes for a given TC and rate code.
    fn qf_bytes(tc: &Timecode, rate_code: u8) -> [u8; 8] {
        [
            (0 << 4) | (tc.f & 0x0F),        // piece 0: frame LS nibble
            (1 << 4) | ((tc.f >> 4) & 0x01), // piece 1: frame MS bit
            (2 << 4) | (tc.s & 0x0F),        // piece 2: sec LS nibble
            (3 << 4) | ((tc.s >> 4) & 0x03), // piece 3: sec MS 2 bits
            (4 << 4) | (tc.m & 0x0F),        // piece 4: min LS nibble
            (5 << 4) | ((tc.m >> 4) & 0x03), // piece 5: min MS 2 bits
            (6 << 4) | (tc.h & 0x0F),        // piece 6: hour LS nibble
            // piece 7: hour MS bit (bit0) + rate code (bits1-2)
            (7 << 4) | (((tc.h >> 4) & 0x01) | ((rate_code & 0x03) << 1)),
        ]
    }

    #[test]
    fn full_sequence_assembles_with_two_frame_offset() {
        // 01:02:03:04 at 30 fps (rate code 3); after +2 frames => 01:02:03:06
        let base = Timecode {
            h: 1,
            m: 2,
            s: 3,
            f: 4,
        };
        let bytes = qf_bytes(&base, 3);
        let mut asm = QfAssembler::new();

        let mut result = None;
        for &b in &bytes {
            result = asm.feed(b);
        }

        let (raw_tc, rate_code) = result.expect("should assemble after 8 pieces");
        assert_eq!(rate_code, 3);
        let (_, fps_n) = rate_code_to_fps(rate_code);
        let corrected = raw_tc.advanced_by(2, fps_n);
        assert_eq!(
            corrected,
            Timecode {
                h: 1,
                m: 2,
                s: 3,
                f: 6
            }
        );
    }

    #[test]
    fn mid_sequence_start_does_not_assemble_early() {
        // Feed pieces 4..7, then a full 0..7 run.  The assembler should only
        // emit a result at the end of the second run.
        let base = Timecode {
            h: 0,
            m: 0,
            s: 0,
            f: 0,
        };
        let bytes = qf_bytes(&base, 0);
        let mut asm = QfAssembler::new();

        // Pieces 4..7: no result expected yet.
        for &b in &bytes[4..8] {
            assert!(
                asm.feed(b).is_none(),
                "should not assemble from a partial sequence"
            );
        }

        // Full run 0..7: piece 0 resets state, so we get a result at piece 7.
        let mut result = None;
        for &b in &bytes[0..8] {
            result = asm.feed(b);
        }
        assert!(
            result.is_some(),
            "should assemble after a complete 0..7 run"
        );
    }

    #[test]
    fn parse_full_frame_sysex() {
        // 02:10:30:15 at 29.97 fps (rate code 2)
        // hh = (2 << 5) | 2 = 0x42
        let msg: &[u8] = &[0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x42, 10, 30, 15, 0xF7];
        let (tc, rate_code) = parse_full_frame(msg).expect("should parse valid full-frame SysEx");
        assert_eq!(rate_code, 2);
        assert_eq!(
            tc,
            Timecode {
                h: 2,
                m: 10,
                s: 30,
                f: 15
            }
        );
    }

    #[test]
    fn parse_full_frame_rejects_invalid() {
        // Too short
        assert!(parse_full_frame(&[0xF0, 0x7F]).is_none());
        // Wrong header byte
        let bad: &[u8] = &[0xF0, 0x7F, 0x00, 0x01, 0x01, 0x02, 10, 30, 15, 0xF7];
        assert!(parse_full_frame(bad).is_none());
        // Missing terminator
        let no_term: &[u8] = &[0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x02, 10, 30, 15, 0x00];
        assert!(parse_full_frame(no_term).is_none());
    }
}
