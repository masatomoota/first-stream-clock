//! SMPTE timecode representation shared by the MTC and LTC receivers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timecode {
    pub h: u8,
    pub m: u8,
    pub s: u8,
    pub f: u8,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_wraps_seconds_and_midnight() {
        let tc = Timecode { h: 23, m: 59, s: 59, f: 29 };
        assert_eq!(tc.advanced_by(1, 30), Timecode { h: 0, m: 0, s: 0, f: 0 });
        let tc = Timecode { h: 1, m: 0, s: 59, f: 25 };
        assert_eq!(tc.advanced_by(10, 30), Timecode { h: 1, m: 1, s: 0, f: 5 });
    }

    #[test]
    fn nominal_rates() {
        assert_eq!(nominal_fps(29.96).1, 30);
        assert_eq!(nominal_fps(25.02).0, 25.0);
        assert_eq!(nominal_fps(24.1).0, 24.0);
    }
}
