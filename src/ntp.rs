//! Minimal SNTP client (RFC 4330). One UDP round-trip, no dependencies.
//! Runs on a background thread and periodically refreshes the clock offset.

use std::net::UdpSocket;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const NTP_EPOCH_OFFSET: f64 = 2_208_988_800.0; // seconds between 1900-01-01 and 1970-01-01
const SYNC_INTERVAL: Duration = Duration::from_secs(64);

#[derive(Default, Clone)]
pub struct NtpStatus {
    /// Seconds to add to the system UTC clock. None until first successful sync.
    pub offset: Option<f64>,
    pub delay_ms: Option<f64>,
    pub last_sync_age_s: Option<u64>,
    pub error: Option<String>,
}

pub enum NtpCmd {
    SetServer(String),
    SyncNow,
}

pub struct NtpHandle {
    pub status: Arc<Mutex<NtpStatusInner>>,
    pub tx: Sender<NtpCmd>,
}

pub struct NtpStatusInner {
    pub offset: Option<f64>,
    pub delay_ms: Option<f64>,
    pub last_sync: Option<Instant>,
    pub error: Option<String>,
}

impl NtpHandle {
    pub fn status(&self) -> NtpStatus {
        let s = self.status.lock().unwrap();
        NtpStatus {
            offset: s.offset,
            delay_ms: s.delay_ms,
            last_sync_age_s: s.last_sync.map(|t| t.elapsed().as_secs()),
            error: s.error.clone(),
        }
    }
}

pub fn spawn(initial_server: String) -> NtpHandle {
    let status = Arc::new(Mutex::new(NtpStatusInner {
        offset: None,
        delay_ms: None,
        last_sync: None,
        error: None,
    }));
    let (tx, rx): (Sender<NtpCmd>, Receiver<NtpCmd>) = std::sync::mpsc::channel();
    let status_bg = Arc::clone(&status);

    std::thread::Builder::new()
        .name("ntp-sync".into())
        .spawn(move || {
            let mut server = initial_server;
            loop {
                match query(&server) {
                    Ok((offset, delay)) => {
                        let mut s = status_bg.lock().unwrap();
                        s.offset = Some(offset);
                        s.delay_ms = Some(delay * 1000.0);
                        s.last_sync = Some(Instant::now());
                        s.error = None;
                    }
                    Err(e) => {
                        status_bg.lock().unwrap().error = Some(e);
                    }
                }
                // Wait for the next cycle, but react to commands immediately.
                match rx.recv_timeout(SYNC_INTERVAL) {
                    Ok(NtpCmd::SetServer(srv)) => server = srv,
                    Ok(NtpCmd::SyncNow) => {}
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawn ntp thread");

    NtpHandle { status, tx }
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn ntp_ts_to_unix(buf: &[u8]) -> f64 {
    let sec = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as f64;
    let frac = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as f64;
    sec - NTP_EPOCH_OFFSET + frac / 4_294_967_296.0
}

/// Returns (clock offset seconds, round-trip delay seconds).
fn query(server: &str) -> Result<(f64, f64), String> {
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let addr = if server.contains(':') {
        server.to_string()
    } else {
        format!("{server}:123")
    };
    sock.connect(&addr).map_err(|e| format!("{addr}: {e}"))?;

    let mut pkt = [0u8; 48];
    pkt[0] = 0x1B; // LI=0, VN=3, Mode=3 (client)

    let t1 = unix_now();
    sock.send(&pkt).map_err(|e| e.to_string())?;
    let mut resp = [0u8; 48];
    let n = sock.recv(&mut resp).map_err(|e| format!("{addr}: {e}"))?;
    let t4 = unix_now();
    if n < 48 {
        return Err(format!("short NTP response ({n} bytes)"));
    }
    let mode = resp[0] & 0x07;
    if mode != 4 && mode != 5 {
        return Err(format!("unexpected NTP mode {mode}"));
    }
    let stratum = resp[1];
    if stratum == 0 {
        return Err("kiss-of-death from server".into());
    }

    let t2 = ntp_ts_to_unix(&resp[32..40]); // server receive
    let t3 = ntp_ts_to_unix(&resp[40..48]); // server transmit
    let offset = ((t2 - t1) + (t3 - t4)) / 2.0;
    let delay = (t4 - t1) - (t3 - t2);
    Ok((offset, delay))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_conversion() {
        // 1900-01-01 + 2208988800s == unix epoch
        let buf = [0x83, 0xAA, 0x7E, 0x80, 0, 0, 0, 0]; // 2208988800 secs
        assert!((ntp_ts_to_unix(&buf) - 0.0).abs() < 1e-9);
        // half-second fraction
        let buf = [0x83, 0xAA, 0x7E, 0x80, 0x80, 0, 0, 0];
        assert!((ntp_ts_to_unix(&buf) - 0.5).abs() < 1e-9);
    }
}
