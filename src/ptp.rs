//! Receive-only PTPv2 (IEEE 1588-2008) listener for AES67 / SMPTE ST 2110 media profiles.
//!
//! Listens on UDP multicast 224.0.1.129 ports 319 (event) and 320 (general).
//! Computes a local-clock UTC offset from Sync/Follow_Up messages.
//!
//! # Offset accuracy note
//! The computed `offset` is a one-way measurement (master transmit time minus local receive
//! time). It therefore includes the network path delay from master to this host. On a well-
//! configured LAN this is sub-millisecond and acceptable for display purposes. A full
//! two-way delay measurement (PDelay or E2E) would require sending packets, which this
//! receive-only implementation deliberately avoids.
//!
//! # TAI / UTC
//! PTP time is TAI. We subtract `currentUtcOffset` (from Announce) to get UTC. If no
//! Announce has been received yet, we fall back to 37 seconds (the value frozen since 2017)
//! and leave `status.utc_offset = None` so the UI can show an assumed value.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ── PTPv2 message type nibbles ────────────────────────────────────────────────
const MSG_SYNC: u8 = 0x0;
const MSG_FOLLOW_UP: u8 = 0x8;
const MSG_ANNOUNCE: u8 = 0xB;

const PTP_VERSION: u8 = 2;

// Flag byte 6, bit mask
const FLAG_TWO_STEP: u8 = 0x02;

// Multicast group used by default PTP domain (AES67 / ST 2110)
const PTP_MULTICAST: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 129);

const EVENT_PORT: u16 = 319;
const GENERAL_PORT: u16 = 320;

/// Fallback TAI-UTC offset in seconds (correct since 2017-01-01, leap-second frozen).
const FALLBACK_UTC_OFFSET_S: i16 = 37;

/// EMA smoothing factor for offset measurements.
const EMA_ALPHA: f64 = 0.2;

/// Maximum pending two-step Sync entries we keep waiting for their Follow_Up.
const MAX_PENDING: usize = 8;

// ── Public types ──────────────────────────────────────────────────────────────

/// Snapshot of the current PTP synchronisation state.
#[derive(Default, Clone)]
pub struct PtpStatus {
    /// Seconds to ADD to system UTC to get PTP-derived UTC.
    /// `None` until the first Sync has been processed.
    pub offset: Option<f64>,
    /// Grandmaster / source clock identity rendered as `XX:XX:XX:XX:XX:XX:XX:XX`.
    pub master: Option<String>,
    /// `currentUtcOffset` (TAI − UTC, e.g. 37) from the most recent Announce message.
    /// `None` if no Announce has been received (fallback of 37 s is used internally).
    pub utc_offset: Option<i16>,
    /// Seconds since the last offset was computed. `None` until first sync.
    pub last_sync_age_s: Option<u64>,
    /// Most recent non-fatal error string (bind failure, short packet, …).
    pub error: Option<String>,
    /// PTP domain number currently being listened to.
    pub domain: u8,
}

/// Handle returned by [`spawn`].  Cheap to clone (Arc inside).
pub struct PtpHandle {
    state: Arc<Mutex<PtpState>>,
    domain: Arc<AtomicU8>,
    /// Bind interface IP packed as u32 big-endian; 0 = UNSPECIFIED (auto).
    bind_ip: Arc<AtomicU32>,
}

impl PtpHandle {
    /// Return a snapshot of the current PTP status.
    pub fn status(&self) -> PtpStatus {
        let s = self.state.lock().unwrap();
        PtpStatus {
            offset: s.ema_offset,
            master: s.master_identity.clone(),
            utc_offset: s.utc_offset,
            last_sync_age_s: s.last_sync.map(|t| t.elapsed().as_secs()),
            error: s.error.clone(),
            domain: self.domain.load(Ordering::Relaxed),
        }
    }

    /// Change the PTP domain to listen on.  Takes effect within ~1 s (socket read timeout).
    pub fn set_domain(&self, domain: u8) {
        self.domain.store(domain, Ordering::Relaxed);
    }

    /// Change the multicast interface. None = OS default (UNSPECIFIED).
    /// Takes effect within ~1 s (socket read timeout triggers socket rebuild).
    pub fn set_interface(&self, ip: Option<Ipv4Addr>) {
        let packed = match ip {
            Some(a) => u32::from(a),
            None => 0,
        };
        self.bind_ip.store(packed, Ordering::Relaxed);
    }
}

// ── Internal shared state ─────────────────────────────────────────────────────

/// A two-step Sync that is waiting for its Follow_Up.
#[derive(Clone)]
struct PendingSync {
    seq_id: u16,
    source_identity: [u8; 8],
    source_port: u16,
    t2_local: f64,
}

struct PtpState {
    ema_offset: Option<f64>,
    master_identity: Option<String>,
    utc_offset: Option<i16>,
    last_sync: Option<Instant>,
    error: Option<String>,
    /// Two-step Sync entries waiting for their Follow_Up (ring-buffer by index).
    pending: Vec<PendingSync>,
}

impl PtpState {
    fn new() -> Self {
        PtpState {
            ema_offset: None,
            master_identity: None,
            utc_offset: None,
            last_sync: None,
            error: None,
            pending: Vec::with_capacity(MAX_PENDING),
        }
    }
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawn two background threads listening on PTP event (319) and general (320) ports.
/// Returns immediately — socket binding happens inside the threads.
pub fn spawn(domain: u8) -> PtpHandle {
    let state = Arc::new(Mutex::new(PtpState::new()));
    let domain_atom = Arc::new(AtomicU8::new(domain));
    let bind_ip_atom = Arc::new(AtomicU32::new(0));

    // Thread: event port 319 — Sync messages
    {
        let state = Arc::clone(&state);
        let domain_atom = Arc::clone(&domain_atom);
        let bind_ip_atom = Arc::clone(&bind_ip_atom);
        std::thread::Builder::new()
            .name("ptp-event".into())
            .spawn(move || event_thread(state, domain_atom, bind_ip_atom))
            .expect("spawn ptp-event thread");
    }

    // Thread: general port 320 — Follow_Up and Announce messages
    {
        let state = Arc::clone(&state);
        let domain_atom = Arc::clone(&domain_atom);
        let bind_ip_atom = Arc::clone(&bind_ip_atom);
        std::thread::Builder::new()
            .name("ptp-general".into())
            .spawn(move || general_thread(state, domain_atom, bind_ip_atom))
            .expect("spawn ptp-general thread");
    }

    PtpHandle {
        state,
        domain: domain_atom,
        bind_ip: bind_ip_atom,
    }
}

// ── Thread bodies ─────────────────────────────────────────────────────────────

fn bind_with_retry(port: u16, state: &Arc<Mutex<PtpState>>) -> UdpSocket {
    loop {
        match UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))) {
            Ok(sock) => {
                // Clear any previous bind-error
                state.lock().unwrap().error = None;
                return sock;
            }
            Err(e) => {
                let msg = format!("port {port} busy: {e}");
                state.lock().unwrap().error = Some(msg);
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

/// Decode a packed u32 bind_ip value to Ipv4Addr (0 → UNSPECIFIED).
fn unpack_ip(raw: u32) -> Ipv4Addr {
    if raw == 0 {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::from(raw)
    }
}

fn setup_socket(sock: &UdpSocket, iface: Ipv4Addr, state: &Arc<Mutex<PtpState>>) {
    if let Err(e) = sock.join_multicast_v4(&PTP_MULTICAST, &iface) {
        state.lock().unwrap().error = Some(format!("multicast join failed: {e}"));
    }
    if let Err(e) = sock.set_read_timeout(Some(Duration::from_secs(1))) {
        state.lock().unwrap().error = Some(format!("set_read_timeout failed: {e}"));
    }
}

fn event_thread(
    state: Arc<Mutex<PtpState>>,
    domain_atom: Arc<AtomicU8>,
    bind_ip_atom: Arc<AtomicU32>,
) {
    let mut current_bind_ip = bind_ip_atom.load(Ordering::Relaxed);
    let sock = bind_with_retry(EVENT_PORT, &state);
    setup_socket(&sock, unpack_ip(current_bind_ip), &state);

    let mut buf = [0u8; 1500];
    let mut sock = sock; // mutable binding for rebuild
    loop {
        // Detect interface change → rebuild socket
        let new_ip = bind_ip_atom.load(Ordering::Relaxed);
        if new_ip != current_bind_ip {
            current_bind_ip = new_ip;
            sock = bind_with_retry(EVENT_PORT, &state);
            setup_socket(&sock, unpack_ip(current_bind_ip), &state);
        }

        let domain = domain_atom.load(Ordering::Relaxed);
        match sock.recv(&mut buf) {
            Ok(n) => {
                if n < 44 {
                    continue; // too short for any meaningful PTP message
                }
                let pkt = &buf[..n];
                let hdr = match parse_header(pkt) {
                    Some(h) => h,
                    None => continue,
                };
                if hdr.domain != domain {
                    continue;
                }
                if hdr.msg_type != MSG_SYNC {
                    continue;
                }
                // T2: local receive time
                let t2_local = unix_now();

                if hdr.two_step {
                    // Stash and wait for Follow_Up
                    let entry = PendingSync {
                        seq_id: hdr.seq_id,
                        source_identity: hdr.source_identity,
                        source_port: hdr.source_port,
                        t2_local,
                    };
                    let mut s = state.lock().unwrap();
                    if s.pending.len() >= MAX_PENDING {
                        s.pending.remove(0);
                    }
                    s.pending.push(entry);
                } else {
                    // One-step: T1 = originTimestamp + correctionField
                    if n < 44 {
                        continue;
                    }
                    let ts = match parse_timestamp(&pkt[34..]) {
                        Some(t) => t,
                        None => continue,
                    };
                    let t1 = ts + correction_secs(hdr.correction_field);
                    let identity_str = format_identity(&hdr.source_identity);
                    let mut s = state.lock().unwrap();
                    apply_offset(&mut s, t1, t2_local, &identity_str);
                }
            }
            Err(ref e) if is_timeout(e) => {
                // normal — loop to check domain/interface change
            }
            Err(e) => {
                state.lock().unwrap().error = Some(format!("recv error port {EVENT_PORT}: {e}"));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn general_thread(
    state: Arc<Mutex<PtpState>>,
    domain_atom: Arc<AtomicU8>,
    bind_ip_atom: Arc<AtomicU32>,
) {
    let mut current_bind_ip = bind_ip_atom.load(Ordering::Relaxed);
    let mut sock = bind_with_retry(GENERAL_PORT, &state);
    setup_socket(&sock, unpack_ip(current_bind_ip), &state);

    let mut buf = [0u8; 1500];
    loop {
        // Detect interface change → rebuild socket
        let new_ip = bind_ip_atom.load(Ordering::Relaxed);
        if new_ip != current_bind_ip {
            current_bind_ip = new_ip;
            sock = bind_with_retry(GENERAL_PORT, &state);
            setup_socket(&sock, unpack_ip(current_bind_ip), &state);
        }

        let domain = domain_atom.load(Ordering::Relaxed);
        match sock.recv(&mut buf) {
            Ok(n) => {
                if n < 44 {
                    continue;
                }
                let pkt = &buf[..n];
                let hdr = match parse_header(pkt) {
                    Some(h) => h,
                    None => continue,
                };
                if hdr.domain != domain {
                    continue;
                }

                match hdr.msg_type {
                    MSG_FOLLOW_UP => {
                        if n < 44 {
                            continue;
                        }
                        let ts = match parse_timestamp(&pkt[34..]) {
                            Some(t) => t,
                            None => continue,
                        };
                        let t1 = ts + correction_secs(hdr.correction_field);
                        let identity_str = format_identity(&hdr.source_identity);

                        let mut s = state.lock().unwrap();
                        // Find the matching pending Sync
                        if let Some(idx) = s.pending.iter().position(|p| {
                            p.seq_id == hdr.seq_id
                                && p.source_identity == hdr.source_identity
                                && p.source_port == hdr.source_port
                        }) {
                            let pending = s.pending.remove(idx);
                            apply_offset(&mut s, t1, pending.t2_local, &identity_str);
                        }
                    }
                    MSG_ANNOUNCE => {
                        if n >= 46 {
                            let utc_offset = i16::from_be_bytes([pkt[44], pkt[45]]);
                            state.lock().unwrap().utc_offset = Some(utc_offset);
                        }
                    }
                    _ => {}
                }
            }
            Err(ref e) if is_timeout(e) => {
                // normal — loop to check domain/interface change
            }
            Err(e) => {
                state.lock().unwrap().error = Some(format!("recv error port {GENERAL_PORT}: {e}"));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// ── Offset computation ────────────────────────────────────────────────────────

/// Apply a new (T1, T2) measurement to shared state, update EMA and master identity.
fn apply_offset(s: &mut PtpState, t1_tai: f64, t2_local: f64, identity: &str) {
    // TAI → UTC
    let tai_utc = s.utc_offset.unwrap_or(FALLBACK_UTC_OFFSET_S) as f64;
    let t1_utc = t1_tai - tai_utc;

    let raw_offset = t1_utc - t2_local;

    // Reset EMA if master changed
    let master_changed = s.master_identity.as_deref() != Some(identity);
    if master_changed {
        s.master_identity = Some(identity.to_string());
        s.ema_offset = None;
    }

    s.ema_offset = Some(match s.ema_offset {
        None => raw_offset,
        Some(prev) => EMA_ALPHA * raw_offset + (1.0 - EMA_ALPHA) * prev,
    });
    s.last_sync = Some(Instant::now());
    s.error = None;
}

// ── Pure parsing helpers (also used by unit tests) ───────────────────────────

/// Decoded PTPv2 common header fields.
pub struct PtpHeader {
    pub msg_type: u8,
    pub domain: u8,
    pub two_step: bool,
    pub correction_field: i64,
    pub source_identity: [u8; 8],
    pub source_port: u16,
    pub seq_id: u16,
}

/// Parse the PTPv2 common header from a raw packet slice.
/// Returns `None` if the packet is too short or the PTP version is not 2.
pub fn parse_header(pkt: &[u8]) -> Option<PtpHeader> {
    if pkt.len() < 34 {
        return None;
    }
    let msg_type = pkt[0] & 0x0F;
    let version = pkt[1] & 0x0F;
    if version != PTP_VERSION {
        return None;
    }
    let domain = pkt[4];
    let flag6 = pkt[6];
    let two_step = (flag6 & FLAG_TWO_STEP) != 0;

    let correction_field = i64::from_be_bytes([
        pkt[8], pkt[9], pkt[10], pkt[11], pkt[12], pkt[13], pkt[14], pkt[15],
    ]);

    let mut source_identity = [0u8; 8];
    source_identity.copy_from_slice(&pkt[20..28]);
    let source_port = u16::from_be_bytes([pkt[28], pkt[29]]);
    let seq_id = u16::from_be_bytes([pkt[30], pkt[31]]);

    Some(PtpHeader {
        msg_type,
        domain,
        two_step,
        correction_field,
        source_identity,
        source_port,
        seq_id,
    })
}

/// Parse a 10-byte PTPv2 timestamp (6-byte seconds + 4-byte nanoseconds) into Unix seconds (f64).
/// The slice must be at least 10 bytes long.
pub fn parse_timestamp(data: &[u8]) -> Option<f64> {
    if data.len() < 10 {
        return None;
    }
    // 6-byte big-endian unsigned seconds (u48)
    let sec = u48_from_be(&data[0..6]);
    let ns = u32::from_be_bytes([data[6], data[7], data[8], data[9]]);
    Some(sec as f64 + ns as f64 * 1e-9)
}

/// Decode a 6-byte big-endian unsigned integer (u48) into u64.
fn u48_from_be(b: &[u8]) -> u64 {
    ((b[0] as u64) << 40)
        | ((b[1] as u64) << 32)
        | ((b[2] as u64) << 24)
        | ((b[3] as u64) << 16)
        | ((b[4] as u64) << 8)
        | (b[5] as u64)
}

/// Convert a raw PTPv2 correctionField (sub-nanoseconds in units of 2^-16 ns) to seconds.
fn correction_secs(raw: i64) -> f64 {
    // raw is ns * 65536; divide by 65536 to get ns, then by 1e9 to get seconds
    (raw as f64) / 65_536.0 / 1_000_000_000.0
}

/// Render an 8-byte clock identity as `XX:XX:XX:XX:XX:XX:XX:XX`.
fn format_identity(id: &[u8; 8]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        id[0], id[1], id[2], id[3], id[4], id[5], id[6], id[7]
    )
}

/// Return the current system time as Unix seconds (f64).
fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Returns `true` if the IO error is a read timeout (WouldBlock / TimedOut).
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Build a minimal valid PTPv2 packet with the given fields.
    // Returns a 44-byte buffer suitable for Sync (one-step) or Follow_Up.
    fn make_packet(
        msg_type: u8,
        domain: u8,
        seq_id: u16,
        two_step: bool,
        correction_raw: i64,
        source_identity: [u8; 8],
        source_port: u16,
        ts_sec: u64,
        ts_ns: u32,
    ) -> Vec<u8> {
        let mut pkt = vec![0u8; 44];
        pkt[0] = msg_type & 0x0F; // messageType in low nibble, transport=0
        pkt[1] = PTP_VERSION & 0x0F; // versionPTP
                                     // messageLength (bytes 2-3): 44
        pkt[2] = 0;
        pkt[3] = 44;
        pkt[4] = domain;
        // flagField byte 6
        pkt[6] = if two_step { FLAG_TWO_STEP } else { 0 };
        // correctionField bytes 8-15
        let cf = correction_raw.to_be_bytes();
        pkt[8..16].copy_from_slice(&cf);
        // sourceClockIdentity bytes 20-27
        pkt[20..28].copy_from_slice(&source_identity);
        // sourcePortNumber bytes 28-29
        let sp = source_port.to_be_bytes();
        pkt[28] = sp[0];
        pkt[29] = sp[1];
        // sequenceId bytes 30-31
        let si = seq_id.to_be_bytes();
        pkt[30] = si[0];
        pkt[31] = si[1];
        // originTimestamp / preciseOriginTimestamp at bytes 34..44 (10 bytes)
        // 6-byte seconds (u48 big-endian)
        pkt[34] = ((ts_sec >> 40) & 0xFF) as u8;
        pkt[35] = ((ts_sec >> 32) & 0xFF) as u8;
        pkt[36] = ((ts_sec >> 24) & 0xFF) as u8;
        pkt[37] = ((ts_sec >> 16) & 0xFF) as u8;
        pkt[38] = ((ts_sec >> 8) & 0xFF) as u8;
        pkt[39] = (ts_sec & 0xFF) as u8;
        // 4-byte nanoseconds
        let ns = ts_ns.to_be_bytes();
        pkt[40..44].copy_from_slice(&ns);
        pkt
    }

    /// Build a 46-byte Announce packet with currentUtcOffset at bytes 44-45.
    fn make_announce(domain: u8, utc_offset: i16) -> Vec<u8> {
        let mut pkt = vec![0u8; 46];
        pkt[0] = MSG_ANNOUNCE & 0x0F;
        pkt[1] = PTP_VERSION & 0x0F;
        pkt[4] = domain;
        let uo = utc_offset.to_be_bytes();
        pkt[44] = uo[0];
        pkt[45] = uo[1];
        pkt
    }

    #[test]
    fn parse_sync_one_step() {
        let identity = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x01, 0x02];
        let pkt = make_packet(
            MSG_SYNC,
            /*domain*/ 0,
            /*seq*/ 42,
            /*two_step*/ false,
            /*correction*/ 0,
            identity,
            /*port*/ 1,
            /*ts_sec*/ 1_700_000_000,
            /*ts_ns*/ 500_000_000,
        );

        let hdr = parse_header(&pkt).expect("header parsed");
        assert_eq!(hdr.msg_type, MSG_SYNC);
        assert_eq!(hdr.domain, 0);
        assert_eq!(hdr.seq_id, 42);
        assert!(!hdr.two_step);
        assert_eq!(hdr.correction_field, 0);
        assert_eq!(hdr.source_identity, identity);
        assert_eq!(hdr.source_port, 1);

        let ts = parse_timestamp(&pkt[34..]).expect("timestamp parsed");
        let expected = 1_700_000_000.0 + 0.5;
        assert!((ts - expected).abs() < 1e-6, "ts={ts} expected={expected}");
    }

    #[test]
    fn parse_follow_up() {
        let identity = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let pkt = make_packet(
            MSG_FOLLOW_UP,
            1,
            99,
            false,
            0,
            identity,
            2,
            1_700_000_100,
            0,
        );

        let hdr = parse_header(&pkt).expect("header");
        assert_eq!(hdr.msg_type, MSG_FOLLOW_UP);
        assert_eq!(hdr.domain, 1);
        assert_eq!(hdr.seq_id, 99);

        let ts = parse_timestamp(&pkt[34..]).expect("timestamp");
        assert!((ts - 1_700_000_100.0).abs() < 1e-9);
    }

    #[test]
    fn parse_announce_utc_offset() {
        let pkt = make_announce(0, 37);
        // Announce header
        let hdr = parse_header(&pkt).expect("header");
        assert_eq!(hdr.msg_type, MSG_ANNOUNCE);
        // currentUtcOffset at bytes 44-45
        let utc_offset = i16::from_be_bytes([pkt[44], pkt[45]]);
        assert_eq!(utc_offset, 37);
    }

    #[test]
    fn u48_exceeds_u32() {
        // Value just above u32::MAX (4_294_967_296 = 0x1_0000_0000)
        let bytes: [u8; 6] = [0x00, 0x01, 0x00, 0x00, 0x00, 0x00];
        let val = u48_from_be(&bytes);
        assert_eq!(val, 0x0001_0000_0000_u64);
        assert!(val > u32::MAX as u64);
    }

    #[test]
    fn correction_field_roundtrip() {
        // 1 ns = 65536 raw units; correction_secs should return 1e-9
        let raw: i64 = 65_536;
        let s = correction_secs(raw);
        assert!((s - 1e-9).abs() < 1e-18, "got {s}");
    }

    #[test]
    fn format_identity_hex() {
        let id = [0xAA, 0xBB, 0x00, 0x11, 0xFF, 0xEE, 0x77, 0x88];
        assert_eq!(format_identity(&id), "AA:BB:00:11:FF:EE:77:88");
    }

    #[test]
    fn two_step_flag_detection() {
        let identity = [0u8; 8];
        let pkt_two = make_packet(MSG_SYNC, 0, 1, true, 0, identity, 1, 0, 0);
        let pkt_one = make_packet(MSG_SYNC, 0, 1, false, 0, identity, 1, 0, 0);
        assert!(parse_header(&pkt_two).unwrap().two_step);
        assert!(!parse_header(&pkt_one).unwrap().two_step);
    }

    #[test]
    fn domain_filter() {
        let identity = [0u8; 8];
        let pkt = make_packet(MSG_SYNC, 5, 1, false, 0, identity, 1, 0, 0);
        let hdr = parse_header(&pkt).unwrap();
        assert_eq!(hdr.domain, 5);
        // Simulate domain mismatch: configured domain is 0
        assert_ne!(hdr.domain, 0u8);
    }

    #[test]
    fn invalid_version_rejected() {
        let identity = [0u8; 8];
        let mut pkt = make_packet(MSG_SYNC, 0, 1, false, 0, identity, 1, 0, 0);
        pkt[1] = 1; // version 1 — should be rejected
        assert!(parse_header(&pkt).is_none());
    }
}
