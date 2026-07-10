//! OSC (Open Sound Control) receiver for the StreamClock Sync Protocol.
//!
//! Listens for `/sc/pos`, `/sc/meta`, and `/sc/bye` datagrams over UDP and
//! exposes the latest snapshot via [`OscReceiver::status`].
//!
//! Also advertises `_streamclock._udp.local.` via mDNS so the bridge plugin
//! can auto-discover this receiver (spec §2).

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mdns_sd::{ServiceDaemon, ServiceInfo};
use rosc::{OscMessage, OscPacket, OscType};

// ---------------------------------------------------------------------------
// Shared state written by the recv thread
// ---------------------------------------------------------------------------

/// Snapshot of the latest `/sc/pos` data (spec §3.1).
#[derive(Clone, Debug, Default)]
pub struct PosSnapshot {
    pub playing: bool,
    pub bar: i32,
    pub beat: i32,
    pub beat_phase: f32,
    pub ppq: f64,
    pub bpm: f32,
    pub ts_num: i32,
    pub ts_den: i32,
    pub tc_hh: i32,
    pub tc_mm: i32,
    pub tc_ss: i32,
    pub tc_ff: i32,
    pub fps: f32,
    pub drop_frame: bool,
    pub seconds: f64,
    /// Wall-clock instant when this snapshot was written.
    pub last_recv_instant: Option<Instant>,
}

/// Supplementary info from `/sc/meta` (spec §3.2).
#[derive(Clone, Debug, Default)]
pub struct MetaSnapshot {
    pub source_name: String,
    pub app_version: String,
}

struct Inner {
    pos: PosSnapshot,
    meta: MetaSnapshot,
    /// Set true by `/sc/bye` (spec §3.3); cleared on next `/sc/pos`.
    bye_received: bool,
    error: Option<String>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            pos: PosSnapshot::default(),
            meta: MetaSnapshot::default(),
            bye_received: false,
            error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Public status snapshot (returned by OscReceiver::status)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OscStatus {
    /// Latest position snapshot; `None` until first `/sc/pos` arrives.
    pub pos: Option<PosSnapshot>,
    /// Supplementary source info from `/sc/meta`.
    pub meta: MetaSnapshot,
    /// UDP port actually bound (SRV port in mDNS).
    pub port: u16,
    /// `true` if a `/sc/pos` arrived within the last 2 seconds (spec §4).
    pub connected: bool,
    /// Last connection-level error message, if any.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// OSC packet parsing helpers (pure functions; tested independently)
// ---------------------------------------------------------------------------

/// Parse a `/sc/pos` OSC message and update `snap`.
///
/// Arg order per spec §3.1 tag string `iiiifdfiiiiiifid` (16 args):
///   0:schemaVer i, 1:playing i, 2:bar i, 3:beat i, 4:beatPhase f,
///   5:ppq d, 6:bpm f, 7:tsNum i, 8:tsDen i, 9:tcHH i, 10:tcMM i,
///   11:tcSS i, 12:tcFF i, 13:fps f, 14:dropFrame i, 15:seconds d
pub fn parse_sc_pos(msg: &OscMessage) -> Option<PosSnapshot> {
    let args = &msg.args;
    if args.len() < 16 {
        return None;
    }

    // Arg 0: schemaVer — must be 1 (spec §3.1 / §6)
    let schema_ver = match &args[0] {
        OscType::Int(v) => *v,
        _ => return None,
    };
    if schema_ver != 1 {
        return None;
    }

    macro_rules! get_int {
        ($idx:expr) => {
            match &args[$idx] {
                OscType::Int(v) => *v,
                _ => return None,
            }
        };
    }
    macro_rules! get_float {
        ($idx:expr) => {
            match &args[$idx] {
                OscType::Float(v) => *v,
                _ => return None,
            }
        };
    }
    macro_rules! get_double {
        ($idx:expr) => {
            match &args[$idx] {
                OscType::Double(v) => *v,
                _ => return None,
            }
        };
    }

    Some(PosSnapshot {
        playing: get_int!(1) != 0,
        bar: get_int!(2),
        beat: get_int!(3),
        beat_phase: get_float!(4),
        ppq: get_double!(5),
        bpm: get_float!(6),
        ts_num: get_int!(7),
        ts_den: get_int!(8),
        tc_hh: get_int!(9),
        tc_mm: get_int!(10),
        tc_ss: get_int!(11),
        tc_ff: get_int!(12),
        fps: get_float!(13),
        drop_frame: get_int!(14) != 0,
        seconds: get_double!(15),
        last_recv_instant: Some(Instant::now()),
    })
}

/// Parse a `/sc/meta` OSC message (spec §3.2).
pub fn parse_sc_meta(msg: &OscMessage) -> Option<MetaSnapshot> {
    let args = &msg.args;
    if args.len() < 3 {
        return None;
    }
    // Arg 0: schemaVer i
    match &args[0] {
        OscType::Int(1) => {}
        _ => return None,
    }
    let source_name = match &args[1] {
        OscType::String(s) => s.clone(),
        _ => return None,
    };
    let app_version = match &args[2] {
        OscType::String(s) => s.clone(),
        _ => return None,
    };
    Some(MetaSnapshot { source_name, app_version })
}

/// Dispatch a decoded OSC message to the inner state.
/// Returns `true` if a `/sc/bye` was received.
fn dispatch(msg: &OscMessage, inner: &mut Inner) -> bool {
    match msg.addr.as_str() {
        "/sc/pos" => {
            if let Some(snap) = parse_sc_pos(msg) {
                inner.pos = snap;
                inner.bye_received = false;
            }
        }
        "/sc/meta" => {
            if let Some(meta) = parse_sc_meta(msg) {
                inner.meta = meta;
            }
        }
        "/sc/bye" => {
            inner.bye_received = true;
            return true;
        }
        _ => {}
    }
    false
}

// ---------------------------------------------------------------------------
// mDNS helpers
// ---------------------------------------------------------------------------

/// Derive a stable 8-hex device ID from the hostname (reproducible, no disk I/O).
fn derive_device_id(hostname: &str) -> String {
    // Simple djb2 hash — stable across restarts, no external crate needed.
    let mut hash: u32 = 5381;
    for b in hostname.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    format!("{:08x}", hash)
}

/// Build a [`ServiceInfo`] for `_streamclock._udp.local.` and register it.
/// Returns the daemon so the caller can keep it alive.
fn advertise_mdns(instance_name: &str, port: u16, device_id: &str) -> Result<ServiceDaemon, String> {
    let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;

    #[cfg(target_os = "windows")]
    let platform = "windows";
    #[cfg(not(target_os = "windows"))]
    let platform = "macos";

    // TXT record per spec §2.2
    let props = [
        ("txtvers", "1"),
        ("proto", "1"),
        ("name", instance_name),
        ("platform", platform),
        ("id", device_id),
    ];

    let service_type = "_streamclock._udp.local.";
    // hostname for SRV record — use local hostname with .local. suffix
    let host = format!("{}.local.", hostname_str());

    let info = ServiceInfo::new(
        service_type,
        instance_name,
        &host,
        "",        // IP — let mdns-sd resolve from host
        port,
        &props[..],
    )
    .map_err(|e| e.to_string())?;

    daemon.register(info).map_err(|e| e.to_string())?;
    Ok(daemon)
}

fn hostname_str() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "streamclock".to_string())
}

// ---------------------------------------------------------------------------
// OSC receiver
// ---------------------------------------------------------------------------

/// Active OSC receiver: binds a UDP socket, spawns a recv thread, and
/// advertises itself over mDNS.
pub struct OscReceiver {
    /// Actual UDP port bound (may differ from 9123 if that was taken).
    port: u16,
    /// State shared with the recv thread.
    inner: Arc<Mutex<Inner>>,
    /// mDNS daemon (kept alive so the advertisement stays active).
    _mdns: Option<ServiceDaemon>,
}

impl OscReceiver {
    /// Bind the UDP socket (default port 9123; falls back to ephemeral),
    /// spawn the recv thread, and start mDNS advertising.
    ///
    /// `instance_name`: human-readable name for mDNS.  `None` → hostname.
    pub fn new(instance_name: Option<String>) -> Result<Self, String> {
        // Bind UDP — prefer 9123, fall back to OS-assigned port.
        let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 9123_u16)))
            .or_else(|_| UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0_u16))))
            .map_err(|e| format!("OSC bind failed: {e}"))?;

        // Read the actual port (important when 9123 was unavailable).
        let port = socket
            .local_addr()
            .map_err(|e| format!("OSC local_addr: {e}"))?
            .port();

        // Set a read timeout so the recv thread can be interrupted on drop.
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .map_err(|e| format!("OSC set_read_timeout: {e}"))?;

        let inner = Arc::new(Mutex::new(Inner::default()));
        let inner_clone = Arc::clone(&inner);

        // Spawn the recv thread.
        std::thread::Builder::new()
            .name("osc-recv".to_string())
            .spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match socket.recv_from(&mut buf) {
                        Ok((n, _src)) => {
                            if let Ok(packet) = rosc::decoder::decode_udp(&buf[..n]) {
                                let mut g =
                                    inner_clone.lock().unwrap_or_else(|e| e.into_inner());
                                handle_packet(packet.1, &mut g);
                            }
                        }
                        Err(ref e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            // Timeout — just loop and re-block.
                        }
                        Err(_) => {
                            // Socket closed or fatal error — exit thread.
                            break;
                        }
                    }
                }
            })
            .map_err(|e| format!("OSC thread spawn failed: {e}"))?;

        // mDNS advertisement — use actual bound port.
        let name = instance_name.unwrap_or_else(hostname_str);
        let host = hostname_str();
        let device_id = derive_device_id(&host);
        let mdns = advertise_mdns(&name, port, &device_id).ok();

        Ok(Self { port, inner, _mdns: mdns })
    }

    /// Poll the current OSC status (non-blocking).
    pub fn status(&self) -> OscStatus {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let has_pos = g.pos.last_recv_instant.is_some();
        let connected = !g.bye_received
            && has_pos
            && g.pos
                .last_recv_instant
                .map(|t| t.elapsed().as_secs_f64() < 2.0)
                .unwrap_or(false);
        OscStatus {
            pos: if has_pos { Some(g.pos.clone()) } else { None },
            meta: g.meta.clone(),
            port: self.port,
            connected,
            error: g.error.clone(),
        }
    }
}

/// Recursively handle an [`OscPacket`] (bundle or message).
fn handle_packet(packet: OscPacket, inner: &mut Inner) {
    match packet {
        OscPacket::Message(msg) => {
            dispatch(&msg, inner);
        }
        OscPacket::Bundle(bundle) => {
            for p in bundle.content {
                handle_packet(p, inner);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// hostname helper shim (avoids pulling in `hostname` crate)
// ---------------------------------------------------------------------------

mod hostname {
    use std::ffi::OsString;

    pub fn get() -> std::io::Result<OsString> {
        #[cfg(unix)]
        {
            let mut buf = [0i8; 256];
            let ret = unsafe { libc_gethostname(buf.as_mut_ptr(), buf.len()) };
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let bytes: Vec<u8> = buf[..len].iter().map(|&c| c as u8).collect();
            Ok(OsString::from(String::from_utf8_lossy(&bytes).into_owned()))
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let mut buf = [0u16; 256];
            let ret = unsafe { GetComputerNameW(buf.as_mut_ptr(), &mut (buf.len() as u32)) };
            if ret == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            Ok(OsString::from_wide(&buf[..len]))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(OsString::from("streamclock"))
        }
    }

    // Thin extern wrappers so we avoid adding a `hostname` crate dependency.
    #[cfg(unix)]
    extern "C" {
        fn gethostname(name: *mut i8, len: usize) -> i32;
    }
    #[cfg(unix)]
    unsafe fn libc_gethostname(name: *mut i8, len: usize) -> i32 {
        gethostname(name, len)
    }

    #[cfg(windows)]
    extern "system" {
        fn GetComputerNameW(lpBuffer: *mut u16, nSize: *mut u32) -> i32;
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::{encoder, OscMessage, OscPacket, OscType};

    /// Build a valid `/sc/pos` OSC packet with the given field values.
    /// Returns the encoded UDP payload bytes.
    fn build_sc_pos_bytes(
        playing: i32,
        bar: i32,
        beat: i32,
        beat_phase: f32,
        ppq: f64,
        bpm: f32,
        ts_num: i32,
        ts_den: i32,
        tc_hh: i32,
        tc_mm: i32,
        tc_ss: i32,
        tc_ff: i32,
        fps: f32,
        drop_frame: i32,
        seconds: f64,
    ) -> Vec<u8> {
        let msg = OscMessage {
            addr: "/sc/pos".to_string(),
            args: vec![
                OscType::Int(1),           // 0: schemaVer
                OscType::Int(playing),     // 1: playing
                OscType::Int(bar),         // 2: bar
                OscType::Int(beat),        // 3: beat
                OscType::Float(beat_phase),// 4: beatPhase
                OscType::Double(ppq),      // 5: ppq
                OscType::Float(bpm),       // 6: bpm
                OscType::Int(ts_num),      // 7: tsNum
                OscType::Int(ts_den),      // 8: tsDen
                OscType::Int(tc_hh),       // 9: tcHH
                OscType::Int(tc_mm),       // 10: tcMM
                OscType::Int(tc_ss),       // 11: tcSS
                OscType::Int(tc_ff),       // 12: tcFF
                OscType::Float(fps),       // 13: fps
                OscType::Int(drop_frame),  // 14: dropFrame
                OscType::Double(seconds),  // 15: seconds
            ],
        };
        encoder::encode(&OscPacket::Message(msg)).expect("encode should succeed")
    }

    // ── Unit test: parse_sc_pos correctness ──────────────────────────────────

    #[test]
    fn parse_sc_pos_decodes_all_fields() {
        let bytes = build_sc_pos_bytes(
            1,     // playing
            5,     // bar
            3,     // beat
            0.75,  // beat_phase
            42.0,  // ppq
            120.5, // bpm
            4,     // ts_num
            4,     // ts_den
            1,     // tc_hh
            2,     // tc_mm
            30,    // tc_ss
            15,    // tc_ff
            29.97, // fps
            0,     // drop_frame
            90.5,  // seconds
        );

        let (_addr, packet) = rosc::decoder::decode_udp(&bytes).expect("decode");
        let msg = match packet {
            OscPacket::Message(m) => m,
            _ => panic!("expected OscPacket::Message"),
        };

        let snap = parse_sc_pos(&msg).expect("parse_sc_pos should succeed");

        assert!(snap.playing);
        assert_eq!(snap.bar, 5);
        assert_eq!(snap.beat, 3);
        assert!((snap.beat_phase - 0.75).abs() < 1e-5, "beat_phase mismatch");
        assert!((snap.ppq - 42.0).abs() < 1e-9, "ppq mismatch");
        assert!((snap.bpm - 120.5).abs() < 1e-3, "bpm mismatch");
        assert_eq!(snap.ts_num, 4);
        assert_eq!(snap.ts_den, 4);
        assert_eq!(snap.tc_hh, 1);
        assert_eq!(snap.tc_mm, 2);
        assert_eq!(snap.tc_ss, 30);
        assert_eq!(snap.tc_ff, 15);
        assert!((snap.fps - 29.97).abs() < 0.01, "fps mismatch");
        assert!(!snap.drop_frame);
        assert!((snap.seconds - 90.5).abs() < 1e-9, "seconds mismatch");
    }

    #[test]
    fn parse_sc_pos_rejects_wrong_schema_ver() {
        let msg = OscMessage {
            addr: "/sc/pos".to_string(),
            args: vec![OscType::Int(2)], // schemaVer = 2
        };
        assert!(parse_sc_pos(&msg).is_none(), "should reject schemaVer != 1");
    }

    #[test]
    fn parse_sc_pos_rejects_too_short() {
        let msg = OscMessage {
            addr: "/sc/pos".to_string(),
            args: vec![OscType::Int(1), OscType::Int(1)], // only 2 args
        };
        assert!(parse_sc_pos(&msg).is_none(), "should reject < 16 args");
    }

    #[test]
    fn parse_sc_pos_playing_false_when_zero() {
        let bytes = build_sc_pos_bytes(0, 1, 1, 0.0, 0.0, 120.0, 4, 4, 0, 0, 0, 0, 30.0, 0, 0.0);
        let (_, packet) = rosc::decoder::decode_udp(&bytes).unwrap();
        let msg = match packet {
            OscPacket::Message(m) => m,
            _ => panic!(),
        };
        let snap = parse_sc_pos(&msg).unwrap();
        assert!(!snap.playing, "playing should be false when arg is 0");
    }

    #[test]
    fn parse_sc_pos_drop_frame_true_when_one() {
        let bytes = build_sc_pos_bytes(1, 1, 1, 0.0, 0.0, 29.97, 4, 4, 0, 0, 0, 0, 29.97, 1, 0.0);
        let (_, packet) = rosc::decoder::decode_udp(&bytes).unwrap();
        let msg = match packet { OscPacket::Message(m) => m, _ => panic!() };
        let snap = parse_sc_pos(&msg).unwrap();
        assert!(snap.drop_frame, "drop_frame should be true when arg is 1");
    }

    // ── Integration-style test: bind receiver, send real UDP datagram ────────

    #[test]
    fn receiver_updates_snapshot_on_real_udp() {
        let recv = OscReceiver::new(None).expect("OscReceiver::new");
        let port = recv.port;

        // Build a datagram and send it via a separate socket.
        let bytes = build_sc_pos_bytes(
            1, 7, 2, 0.5, 24.0, 140.0, 3, 4, 0, 1, 30, 0, 30.0, 0, 90.0,
        );
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("sender bind");
        sender
            .send_to(&bytes, format!("127.0.0.1:{port}"))
            .expect("send_to");

        // Give the recv thread time to process.
        std::thread::sleep(std::time::Duration::from_millis(100));

        let status = recv.status();
        assert!(status.connected, "should be connected after receiving /sc/pos");
        let pos = status.pos.expect("pos snapshot should be set");
        assert_eq!(pos.bar, 7);
        assert_eq!(pos.beat, 2);
        assert!((pos.bpm - 140.0).abs() < 0.01);
        assert_eq!(pos.ts_num, 3);
        assert_eq!(pos.ts_den, 4);
    }

    // ── derive_device_id is stable ───────────────────────────────────────────

    #[test]
    fn device_id_is_8_hex_chars() {
        let id = derive_device_id("my-macbook");
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "id should be hex");
        // Stability: same hostname → same id
        assert_eq!(id, derive_device_id("my-macbook"));
    }

    // ── /sc/meta parsing ─────────────────────────────────────────────────────

    #[test]
    fn parse_sc_meta_decodes_name_and_version() {
        let msg = OscMessage {
            addr: "/sc/meta".to_string(),
            args: vec![
                OscType::Int(1),
                OscType::String("MacStudio / Cubase".to_string()),
                OscType::String("1.0.0".to_string()),
                OscType::Int(48000),
            ],
        };
        let meta = parse_sc_meta(&msg).expect("parse_sc_meta");
        assert_eq!(meta.source_name, "MacStudio / Cubase");
        assert_eq!(meta.app_version, "1.0.0");
    }
}
