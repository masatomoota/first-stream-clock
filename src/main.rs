#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tc;
mod ntp;
mod mtc;
mod ltc;
mod ptp;

use eframe::egui::{
    self, CentralPanel, Color32, FontFamily, FontId, Key, Label, Rect, RichText, Sense, Vec2,
};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

// ── NIC helpers ──────────────────────────────────────────────────────────────

/// Return (interface_name, IPv4 address) for all non-loopback IPv4 interfaces.
fn list_nics() -> Vec<(String, Ipv4Addr)> {
    match if_addrs::get_if_addrs() {
        Err(_) => Vec::new(),
        Ok(addrs) => addrs
            .into_iter()
            .filter_map(|iface| {
                if iface.is_loopback() {
                    return None;
                }
                match iface.addr {
                    if_addrs::IfAddr::V4(ref v4) => Some((iface.name.clone(), v4.ip)),
                    _ => None,
                }
            })
            .collect(),
    }
}

/// Detect the default-route NIC IP using the UDP connect trick (no packets sent).
fn default_route_ip() -> Option<Ipv4Addr> {
    use std::net::UdpSocket;
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()? {
        std::net::SocketAddr::V4(v4) => Some(*v4.ip()),
        _ => None,
    }
}

// ── Settings ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, PartialEq)]
enum Source {
    System,
    Ntp,
    Ptp,
    Mtc,
    Ltc,
}

impl Default for Source {
    fn default() -> Self {
        Source::System
    }
}

fn default_local_fps() -> f32 {
    30.0
}

fn default_text_color() -> [u8; 3] {
    [0x00, 0xFF, 0x66]
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
enum FontStyle {
    Modern,
    SevenSeg,
}

impl Default for FontStyle {
    fn default() -> Self {
        FontStyle::Modern
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Settings {
    source: Source,
    bg_alpha: f32,
    topmost: bool,
    ntp_server: String,
    ptp_domain: u8,
    mtc_port: Option<String>,
    ltc_device: Option<String>,
    #[serde(default)]
    show_frames: bool,
    #[serde(default = "default_local_fps")]
    local_fps: f32,
    #[serde(default = "default_text_color")]
    text_color: [u8; 3],
    #[serde(default)]
    font_style: FontStyle,
    /// Minimize to taskbar on close instead of quitting.
    #[serde(default)]
    minimize_on_close: bool,
    /// Local IPv4 as string for NTP bind; None = auto (default-route NIC).
    #[serde(default)]
    ntp_nic: Option<String>,
    /// Local IPv4 as string for PTP multicast interface; None = auto.
    #[serde(default)]
    ptp_nic: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            source: Source::System,
            bg_alpha: 1.0,
            topmost: true,
            ntp_server: "ntp.nict.jp".to_string(),
            ptp_domain: 0,
            mtc_port: None,
            ltc_device: None,
            show_frames: false,
            local_fps: 30.0,
            text_color: default_text_color(),
            font_style: FontStyle::Modern,
            minimize_on_close: false,
            ntp_nic: None,
            ptp_nic: None,
        }
    }
}

// ── Stopwatch ───────────────────────────────────────────────────────────────

struct Stopwatch {
    acc: Duration,
    since: Option<Instant>,
}

/// Phase-aligned display seconds: increments only at clock-source flip boundaries.
/// acc_secs: whole seconds accumulated from previous segments.
/// e_seg: wall-clock seconds elapsed in the current running segment.
/// phase: sub-second fraction [0,1) of the active time source.
fn aligned_display_secs(acc_secs: u64, e_seg: f64, phase: f64) -> u64 {
    let seg_term = ((e_seg - phase + 0.5).floor() as i64).max(0) as u64;
    acc_secs + seg_term
}

/// Format a total-seconds count as HH:MM:SS.
fn format_hms(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

impl Stopwatch {
    fn new() -> Self {
        Self { acc: Duration::ZERO, since: None }
    }

    fn elapsed(&self) -> Duration {
        match self.since {
            Some(t) => self.acc + t.elapsed(),
            None => self.acc,
        }
    }

    fn is_running(&self) -> bool {
        self.since.is_some()
    }

    fn start(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }

    /// Stop and freeze display at exactly display_secs (whole seconds, no visible jump).
    fn stop_at(&mut self, display_secs: u64) {
        self.since = None;
        self.acc = Duration::from_secs(display_secs);
    }

    fn reset(&mut self) {
        self.since = None;
        self.acc = Duration::ZERO;
    }

    /// Double-click cycle: running→stop_at, stopped-with-time→reset, zero→start.
    fn cycle(&mut self, display_secs: u64) {
        if self.is_running() {
            self.stop_at(display_secs);
        } else if self.acc > Duration::ZERO {
            self.reset();
        } else {
            self.start();
        }
    }

    /// Phase-aligned display value in whole seconds.
    /// phase: sub-second fraction [0,1) of the active clock source.
    fn display_secs(&self, phase: f64) -> u64 {
        if let Some(t) = self.since {
            aligned_display_secs(self.acc.as_secs(), t.elapsed().as_secs_f64(), phase)
        } else {
            self.acc.as_secs()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_zero_before_first_flip() {
        // Started when phase was 0, now e_seg=0.3, phase=0.3 → segment term = floor(0.3-0.3+0.5)=0
        assert_eq!(aligned_display_secs(0, 0.3, 0.3), 0);
        // e_seg=0.7, phase=0.7 → floor(0.7-0.7+0.5)=0
        assert_eq!(aligned_display_secs(0, 0.7, 0.7), 0);
    }

    #[test]
    fn aligned_increments_at_flip() {
        // Just after the clock flipped: phase≈0.0, e_seg=0.7 (offset stayed ~0.7)
        // floor(0.7-0.0+0.5) = floor(1.2) = 1 → D=1
        assert_eq!(aligned_display_secs(0, 0.7, 0.0), 1);
    }

    #[test]
    fn aligned_clamp_negative_segment_term() {
        // acc=5, e_seg=0.05, phase=0.9 → floor(0.05-0.9+0.5)=floor(-0.35)=-1 → clamped 0 → D=5
        assert_eq!(aligned_display_secs(5, 0.05, 0.9), 5);
    }

    #[test]
    fn aligned_increments_only_at_flip() {
        // Constant offset (started at flip): e_seg and phase advance together.
        // D should stay 0 until phase wraps to 0 and e_seg is ~1.0.
        let offset = 0.0_f64; // started exactly at flip
        for tick in 0..95_u32 {
            let t = tick as f64 * 0.01; // 0.00 .. 0.94
            let e_seg = offset + t;
            let phase = t;
            let d = aligned_display_secs(0, e_seg, phase);
            assert_eq!(d, 0, "expected 0 at e_seg={e_seg:.2} phase={phase:.2}");
        }
        // At e_seg=1.0, phase=0.0 (just after flip): should be 1
        assert_eq!(aligned_display_secs(0, 1.0, 0.0), 1);
    }

    #[test]
    fn aligned_accumulator_carries() {
        // acc=10 already accumulated, segment adds another 2 flips worth
        assert_eq!(aligned_display_secs(10, 2.3, 0.3), 12);
    }

    #[test]
    fn list_nics_does_not_panic() {
        // Should return without panic; result may be empty in CI but must not panic.
        let nics = list_nics();
        // All returned IPs must be non-loopback IPv4.
        for (_name, ip) in &nics {
            assert!(!ip.is_loopback(), "loopback slipped through: {ip}");
        }
    }

    #[test]
    fn default_route_ip_plausible() {
        // Should return Some on a machine with network; must not panic.
        let ip = default_route_ip();
        if let Some(ip) = ip {
            // Must not be loopback or unspecified
            assert!(!ip.is_loopback(), "default route is loopback: {ip}");
            assert!(!ip.is_unspecified(), "default route is unspecified");
        }
        // None is acceptable in CI without networking.
    }
}

// ── App ─────────────────────────────────────────────────────────────────────

struct App {
    settings: Settings,
    stopwatch: Stopwatch,
    ntp: ntp::NtpHandle,
    ptp: ptp::PtpHandle,
    mtc: mtc::MtcReceiver,
    ltc: ltc::LtcReceiver,

    settings_open: bool,

    /// Set true before sending Close from the context-menu "Exit" action
    /// so the minimize-on-close interception does not intercept it.
    force_exit: bool,

    // transient UI state for settings window
    ntp_server_edit: String,
    mtc_ports: Vec<String>,
    mtc_selected: String,
    ltc_devices: Vec<String>,
    ltc_selected: String,

    // NIC list for NTP/PTP interface selection (cached; refreshed on settings open / Refresh click)
    nic_list: Vec<(String, Ipv4Addr)>,
    default_ip: Option<Ipv4Addr>,
    // Currently-selected display strings for the combos
    ntp_nic_selected: String,
    ptp_nic_selected: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();

        // Force dark theme so settings panel is always readable
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        // Register DSEG7 Classic Bold for the 7-segment font style
        {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "dseg7".to_owned(),
                egui::FontData::from_static(include_bytes!("../assets/fonts/DSEG7Classic-Bold.ttf")).into(),
            );
            fonts.families.insert(
                FontFamily::Name("dseg7".into()),
                vec!["dseg7".to_owned(), "Hack".to_owned()],
            );
            cc.egui_ctx.set_fonts(fonts);
        }

        let ntp = ntp::spawn(settings.ntp_server.clone());
        // Apply saved NTP NIC if any
        if let Some(ref s) = settings.ntp_nic {
            if let Ok(ip) = s.parse::<Ipv4Addr>() {
                let _ = ntp.tx.send(ntp::NtpCmd::SetBindIp(Some(ip)));
            }
        }

        let ptp = ptp::spawn(settings.ptp_domain);
        // Apply saved PTP NIC if any
        if let Some(ref s) = settings.ptp_nic {
            if let Ok(ip) = s.parse::<Ipv4Addr>() {
                ptp.set_interface(Some(ip));
            }
        }

        let mut mtc = mtc::MtcReceiver::new();
        if let Some(ref port) = settings.mtc_port {
            let _ = mtc.connect(port);
        }

        let mut ltc = ltc::LtcReceiver::new();
        if let Some(ref dev) = settings.ltc_device {
            let device_arg: Option<&str> = Some(dev.as_str());
            let _ = ltc.connect(device_arg);
        }

        let ntp_server_edit = settings.ntp_server.clone();

        // Build initial NIC display strings from persisted settings
        let ntp_nic_selected = settings.ntp_nic.clone().unwrap_or_default();
        let ptp_nic_selected = settings.ptp_nic.clone().unwrap_or_default();

        Self {
            settings,
            stopwatch: Stopwatch::new(),
            ntp,
            ptp,
            mtc,
            ltc,
            settings_open: false,
            force_exit: false,
            ntp_server_edit,
            mtc_ports: Vec::new(),
            mtc_selected: String::new(),
            ltc_devices: Vec::new(),
            ltc_selected: String::new(),
            nic_list: Vec::new(),
            default_ip: None,
            ntp_nic_selected,
            ptp_nic_selected,
        }
    }

    /// Refresh NIC list and default-route IP (call on settings open or Refresh click).
    fn refresh_nics(&mut self) {
        self.nic_list = list_nics();
        self.default_ip = default_route_ip();
    }
}

// ── Time helpers ─────────────────────────────────────────────────────────────

fn jst_now_with_offset(offset_secs: f64) -> chrono::DateTime<chrono::FixedOffset> {
    use chrono::{Duration as CDuration, FixedOffset, Utc};
    let jst = FixedOffset::east_opt(9 * 3600).expect("valid JST offset");
    let utc = Utc::now();
    let micros = (offset_secs * 1_000_000.0) as i64;
    let adjusted = utc + CDuration::microseconds(micros);
    adjusted.with_timezone(&jst)
}

fn jst_now() -> chrono::DateTime<chrono::FixedOffset> {
    jst_now_with_offset(0.0)
}

// ── Colors ───────────────────────────────────────────────────────────────────

const AMBER: Color32 = Color32::from_rgb(0xFF, 0xB3, 0x00);
const RED_SIG: Color32 = Color32::from_rgb(0xFF, 0x55, 0x44);

// ── eframe::App ──────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    // eframe 0.34: `logic` is called before `ui` each frame; handle keyboard
    // input and schedule the next repaint here (no painting allowed).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Minimize-on-close interception ──────────────────────────────────
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.settings.minimize_on_close && !self.force_exit {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            // If force_exit is true, let the close proceed normally.
        }

        // ── Keyboard input ──────────────────────────────────────────────────
        ctx.input(|i| {
            if i.key_pressed(Key::Escape) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if i.key_pressed(Key::ArrowUp) {
                self.settings.bg_alpha = (self.settings.bg_alpha + 0.05).min(1.0);
            }
            if i.key_pressed(Key::ArrowDown) {
                self.settings.bg_alpha = (self.settings.bg_alpha - 0.05).max(0.0);
            }
            // Mouse wheel (egui 0.34: raw_scroll_delta removed; use smooth_scroll_delta)
            let scroll = i.smooth_scroll_delta.y;
            if scroll != 0.0 {
                let sign = scroll.signum();
                self.settings.bg_alpha = (self.settings.bg_alpha + 0.05 * sign).clamp(0.0, 1.0);
            }
        });

        // Repaint at ~20 fps (minimal CPU for long streaming sessions)
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    // eframe 0.34: `ui` is the primary paint entry point; receives the root Ui.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Obtain the context for viewport commands and window spawning.
        let ctx = ui.ctx().clone();

        // ── Compute current time strings ────────────────────────────────────
        let ntp_status = self.ntp.status();
        let ptp_status = self.ptp.status();
        let mtc_status = self.mtc.status();
        let ltc_status = self.ltc.status();

        // Derive display colors from text_color setting (bright/dim/dark variants)
        let [tr, tg, tb] = self.settings.text_color;
        let col_bright = Color32::from_rgb(tr, tg, tb);
        let col_dim = Color32::from_rgb(
            (tr as f32 * 0.55) as u8,
            (tg as f32 * 0.55) as u8,
            (tb as f32 * 0.55) as u8,
        );
        let col_dark = Color32::from_rgb(
            (tr as f32 * 0.35) as u8,
            (tg as f32 * 0.35) as u8,
            (tb as f32 * 0.35) as u8,
        );

        // Date always from system JST
        let sys_jst = jst_now();
        // In SevenSeg mode use dashes (DSEG7 has no slash glyph)
        let date_str = if self.settings.font_style == FontStyle::SevenSeg {
            sys_jst.format("%Y-%m-%d").to_string()
        } else {
            sys_jst.format("%Y/%m/%d").to_string()
        };

        // Time row: depends on source; also yields phase (sub-second fraction [0,1)) for stopwatch alignment.
        let (time_str, time_color, status_str, status_color, phase) = match &self.settings.source {
            Source::System => {
                let dt = jst_now();
                let ph = dt.timestamp_subsec_micros() as f64 * 1e-6;
                let t = if self.settings.show_frames {
                    let fps = self.settings.local_fps;
                    let ff = ((ph * fps as f64).floor() as u32).min(fps.ceil() as u32 - 1);
                    format!("{}:{:02}", dt.format("%H:%M:%S"), ff)
                } else {
                    dt.format("%H:%M:%S").to_string()
                };
                let st = "SYS JST".to_string();
                (t, col_bright, st, col_dim, ph)
            }
            Source::Ntp => {
                let offset = ntp_status.offset.unwrap_or(0.0);
                let dt = jst_now_with_offset(offset);
                let ph = dt.timestamp_subsec_micros() as f64 * 1e-6;
                let t = if self.settings.show_frames {
                    let fps = self.settings.local_fps;
                    let ff = ((ph * fps as f64).floor() as u32).min(fps.ceil() as u32 - 1);
                    format!("{}:{:02}", dt.format("%H:%M:%S"), ff)
                } else {
                    dt.format("%H:%M:%S").to_string()
                };
                let st = match ntp_status.offset {
                    Some(off) => {
                        let server = &self.settings.ntp_server;
                        let delay = ntp_status.delay_ms.unwrap_or(0.0);
                        let age = ntp_status.last_sync_age_s.unwrap_or(0);
                        format!("NTP {} {:+.3}s d={:.0}ms {}s ago", server, off, delay, age)
                    }
                    None => {
                        let err = ntp_status.error.as_deref().unwrap_or("connecting");
                        format!("NTP no sync yet: {}", err)
                    }
                };
                (t, col_bright, st, col_dim, ph)
            }
            Source::Ptp => {
                let offset = ptp_status.offset.unwrap_or(0.0);
                let dt = jst_now_with_offset(offset);
                let ph = dt.timestamp_subsec_micros() as f64 * 1e-6;
                let t = if self.settings.show_frames {
                    let fps = self.settings.local_fps;
                    let ff = ((ph * fps as f64).floor() as u32).min(fps.ceil() as u32 - 1);
                    format!("{}:{:02}", dt.format("%H:%M:%S"), ff)
                } else {
                    dt.format("%H:%M:%S").to_string()
                };
                let st = match ptp_status.offset {
                    Some(_) => {
                        let master = ptp_status.master.as_deref().unwrap_or("?");
                        let utc_off = ptp_status.utc_offset.unwrap_or(0);
                        let age = ptp_status.last_sync_age_s.unwrap_or(0);
                        let dom = ptp_status.domain;
                        format!("PTP dom{} master {} utc{:+} {}s ago", dom, master, utc_off, age)
                    }
                    None => {
                        let err = ptp_status.error.as_deref().unwrap_or("no master");
                        format!("PTP {}", err)
                    }
                };
                (t, col_bright, st, col_dim, ph)
            }
            Source::Mtc => {
                // Freewheel MTC timecode
                let age = mtc_status.age;
                let fps_label = mtc_status.fps_label;
                let fps_n = mtc_status.fps_n;
                let port = mtc_status.port.as_deref().unwrap_or("?");

                let (t, tc_color, st, st_color, ph) = match mtc_status.tc {
                    Some(tc) => {
                        match age {
                            Some(a) if a < Duration::from_secs(2) => {
                                // Freewheel: advance by elapsed frames
                                let extra_frames = (a.as_secs_f64() * fps_label as f64) as u64;
                                let live = tc.advanced_by(extra_frames, fps_n);
                                let t = if self.settings.show_frames { live.hmsf() } else { live.hms() };
                                let st = format!("MTC {} {:.2}fps {}", port, fps_label, live.hmsf());
                                // Phase = fraction of the current second in the freewheeled timecode
                                let ph = ((live.f as f64 + (a.as_secs_f64() * fps_label as f64).fract()) / fps_label as f64).clamp(0.0, 0.999);
                                (t, col_bright, st, col_dim, ph)
                            }
                            _ => {
                                // No signal: hold last value; fall back to system phase
                                let t = if self.settings.show_frames { tc.hmsf() } else { tc.hms() };
                                let st = format!("MTC NO SIGNAL (last {})", tc.hmsf());
                                let ph = jst_now().timestamp_subsec_micros() as f64 * 1e-6;
                                (t, RED_SIG, st, RED_SIG, ph)
                            }
                        }
                    }
                    None => {
                        let err = mtc_status.error.as_deref().unwrap_or("no signal");
                        let ph = jst_now().timestamp_subsec_micros() as f64 * 1e-6;
                        ("--:--:--".to_string(), RED_SIG, format!("MTC {}", err), RED_SIG, ph)
                    }
                };
                (t, tc_color, st, st_color, ph)
            }
            Source::Ltc => {
                let age = ltc_status.age;
                let fps_label = ltc_status.fps_label;
                let fps_n = ltc_status.fps_n;
                let device = ltc_status.device.as_deref().unwrap_or("?");

                let (t, tc_color, st, st_color, ph) = match ltc_status.tc {
                    Some(tc) => {
                        match age {
                            Some(a) if a < Duration::from_secs(2) => {
                                let extra_frames = (a.as_secs_f64() * fps_label as f64) as u64;
                                let live = tc.advanced_by(extra_frames, fps_n);
                                let t = if self.settings.show_frames { live.hmsf() } else { live.hms() };
                                let st = format!("LTC {} {:.2}fps {}", device, fps_label, live.hmsf());
                                let ph = ((live.f as f64 + (a.as_secs_f64() * fps_label as f64).fract()) / fps_label as f64).clamp(0.0, 0.999);
                                (t, col_bright, st, col_dim, ph)
                            }
                            _ => {
                                let t = if self.settings.show_frames { tc.hmsf() } else { tc.hms() };
                                let st = format!("LTC NO SIGNAL (last {})", tc.hmsf());
                                let ph = jst_now().timestamp_subsec_micros() as f64 * 1e-6;
                                (t, RED_SIG, st, RED_SIG, ph)
                            }
                        }
                    }
                    None => {
                        let err = ltc_status.error.as_deref().unwrap_or("no signal");
                        let ph = jst_now().timestamp_subsec_micros() as f64 * 1e-6;
                        ("--:--:--".to_string(), RED_SIG, format!("LTC {}", err), RED_SIG, ph)
                    }
                };
                (t, tc_color, st, st_color, ph)
            }
        };

        // Stopwatch: phase-aligned display (increments only at clock-source second flips)
        let sw_secs = self.stopwatch.display_secs(phase);
        let sw_str = format_hms(sw_secs);
        let sw_color = if self.stopwatch.is_running() {
            col_bright
        } else if self.stopwatch.elapsed() > Duration::ZERO {
            AMBER
        } else {
            col_dark
        };

        // ── Main panel ──────────────────────────────────────────────────────
        // egui 0.34: CentralPanel::show(ctx) is deprecated; use show_inside(ui).
        CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                let avail = ui.available_size();

                // Paint black background with variable alpha
                let alpha_byte = (self.settings.bg_alpha * 255.0) as u8;
                let bg_color = Color32::from_black_alpha(alpha_byte);
                let rect = Rect::from_min_size(ui.min_rect().min, avail);
                let painter = ui.painter();
                painter.rect_filled(rect, 0.0, bg_color);

                // Feature D: paint thin border when background is nearly transparent
                // and the pointer is over the window or a drag is in progress.
                if self.settings.bg_alpha < 0.05 {
                    let ptr_in_window = ctx
                        .pointer_hover_pos()
                        .map(|p| rect.contains(p))
                        .unwrap_or(false);
                    let dragging = ctx.input(|i| i.pointer.any_down());
                    if ptr_in_window || dragging {
                        painter.rect_stroke(
                            rect.shrink(1.0),
                            0.0,
                            egui::Stroke::new(1.5, Color32::from_gray(110)),
                            egui::StrokeKind::Middle,
                        );
                    }
                }

                // Scale factor
                let s = (avail.x / 480.0).min(avail.y / 320.0);

                // Font sizes (SevenSeg glyphs are wider; apply 0.80 factor to fit)
                let seg_scale = if self.settings.font_style == FontStyle::SevenSeg { 0.80 } else { 1.0 };
                let sz_date = 26.0 * s * seg_scale;
                let time_scale = if self.settings.show_frames { 0.72 } else { 1.0 };
                let sz_time = 92.0 * s * time_scale * seg_scale;
                let sz_sw = 70.0 * s * seg_scale;
                let sz_status = 12.0 * s;

                // Estimate total block height (no exact measure without layout pass,
                // use the font sizes as proxy — monospace line height ≈ font size * 1.2)
                let line_gap = 4.0 * s;
                let total_h = sz_date * 1.2
                    + line_gap
                    + sz_time * 1.2
                    + line_gap
                    + sz_sw * 1.2
                    + line_gap
                    + sz_status * 1.2;
                let top_pad = ((avail.y - total_h) / 2.0).max(0.0);

                // Full-window drag/interact target (behind text)
                let bg_response = ui.interact(
                    Rect::from_min_size(ui.min_rect().min, avail),
                    ui.id().with("bg_drag"),
                    Sense::click_and_drag(),
                );

                if bg_response.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                // Context menu on background
                bg_response.context_menu(|ui| {
                    ui.label("Time source");
                    ui.separator();
                    if ui.radio(self.settings.source == Source::System, "System").clicked() {
                        self.settings.source = Source::System;
                        ui.close();
                    }
                    if ui.radio(self.settings.source == Source::Ntp, "NTP").clicked() {
                        self.settings.source = Source::Ntp;
                        ui.close();
                    }
                    if ui.radio(self.settings.source == Source::Ptp, "PTP").clicked() {
                        self.settings.source = Source::Ptp;
                        ui.close();
                    }
                    if ui.radio(self.settings.source == Source::Mtc, "MTC").clicked() {
                        self.settings.source = Source::Mtc;
                        ui.close();
                    }
                    if ui.radio(self.settings.source == Source::Ltc, "LTC").clicked() {
                        self.settings.source = Source::Ltc;
                        ui.close();
                    }
                    ui.separator();
                    let topmost_label = if self.settings.topmost {
                        "Always on top [ON]"
                    } else {
                        "Always on top [OFF]"
                    };
                    if ui.checkbox(&mut self.settings.topmost, topmost_label).clicked() {
                        let level = if self.settings.topmost {
                            egui::viewport::WindowLevel::AlwaysOnTop
                        } else {
                            egui::viewport::WindowLevel::Normal
                        };
                        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                        ui.close();
                    }
                    if ui.button("Settings...").clicked() {
                        self.settings_open = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        // Force-exit: bypass minimize-on-close interception
                        self.force_exit = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                // Choose font family for date/time/stopwatch rows
                let clock_family = match self.settings.font_style {
                    FontStyle::Modern => FontFamily::Monospace,
                    FontStyle::SevenSeg => FontFamily::Name("dseg7".into()),
                };

                // ── Clock rows (vertically centered) ──
                ui.add_space(top_pad);

                // Row 1: Date
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.label(
                                RichText::new(&date_str)
                                    .font(FontId::new(sz_date, clock_family.clone()))
                                    .color(col_bright),
                            );
                        },
                    );
                });

                ui.add_space(line_gap);

                // Row 2: Time
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.label(
                                RichText::new(&time_str)
                                    .font(FontId::new(sz_time, clock_family.clone()))
                                    .color(time_color),
                            );
                        },
                    );
                });

                ui.add_space(line_gap);

                // Row 3: Stopwatch (clickable for double-click cycling)
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            let sw_label = Label::new(
                                RichText::new(&sw_str)
                                    .font(FontId::new(sz_sw, clock_family.clone()))
                                    .color(sw_color),
                            )
                            .sense(Sense::click());
                            let sw_response = ui.add(sw_label);
                            if sw_response.double_clicked() {
                                self.stopwatch.cycle(sw_secs);
                            }
                        },
                    );
                });

                ui.add_space(line_gap);

                // Row 4: Status line
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.label(
                                RichText::new(&status_str)
                                    .font(FontId::monospace(sz_status))
                                    .color(status_color),
                            );
                        },
                    );
                });

                // ── Resize grip (bottom-right corner) ──────────────────────
                let grip_size = 18.0;
                let grip_rect = Rect::from_min_size(
                    egui::Pos2::new(
                        ui.min_rect().min.x + avail.x - grip_size,
                        ui.min_rect().min.y + avail.y - grip_size,
                    ),
                    Vec2::splat(grip_size),
                );
                let grip_response = ui.interact(
                    grip_rect,
                    ui.id().with("resize_grip"),
                    Sense::drag(),
                );
                if grip_response.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(
                        egui::ResizeDirection::SouthEast,
                    ));
                }
                // Draw subtle diagonal lines only when pointer is over the window
                let pointer_in_window = ctx.input(|i| i.pointer.has_pointer());
                if pointer_in_window {
                    let p = ui.painter();
                    let dim_green = Color32::from_rgba_unmultiplied(0x1F, 0x7A, 0x3D, 180);
                    let stroke = egui::Stroke::new(1.0, dim_green);
                    let br = grip_rect.max;
                    for offset in [4.0_f32, 8.0, 12.0] {
                        p.line_segment(
                            [
                                egui::Pos2::new(br.x - offset, br.y),
                                egui::Pos2::new(br.x, br.y - offset),
                            ],
                            stroke,
                        );
                    }
                }
            });

        // ── Settings window ─────────────────────────────────────────────────
        if self.settings_open {
            // Refresh device/port lists and NIC list while settings window is first opened.
            // (Only refresh on the frame it becomes open, or on explicit Refresh click.)
            self.mtc_ports = mtc::MtcReceiver::list_ports();
            self.ltc_devices = {
                let mut devs = ltc::LtcReceiver::list_devices();
                devs.insert(0, "(default)".to_string());
                devs
            };
            // Refresh NIC list once per frame while settings open (cheap after first call)
            if self.nic_list.is_empty() {
                self.refresh_nics();
            }

            // Build "Auto" label string for NIC combos
            let auto_label = match self.default_ip {
                Some(ip) => format!("Auto (default route: {ip})"),
                None => "Auto (default route: unknown)".to_string(),
            };

            let mut open = self.settings_open;
            egui::Window::new("Settings")
                .open(&mut open)
                .resizable(true)
                // Feature C: explicit near-black frame so panel is readable regardless of OS theme
                .frame(
                    egui::Frame::window(&ctx.global_style())
                        .fill(Color32::from_rgb(18, 18, 18)),
                )
                .show(&ctx, |ui| {
                    // Make all text in this panel readable on dark backgrounds
                    ui.visuals_mut().override_text_color = Some(egui::Color32::WHITE);

                    // Source selection
                    ui.heading("Time Source");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.settings.source, Source::System, "System");
                        ui.radio_value(&mut self.settings.source, Source::Ntp, "NTP");
                        ui.radio_value(&mut self.settings.source, Source::Ptp, "PTP");
                        ui.radio_value(&mut self.settings.source, Source::Mtc, "MTC");
                        ui.radio_value(&mut self.settings.source, Source::Ltc, "LTC");
                    });

                    ui.separator();

                    // NTP settings
                    ui.collapsing("NTP", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Server:");
                            ui.text_edit_singleline(&mut self.ntp_server_edit);
                        });
                        if ui.button("Apply & sync now").clicked() {
                            self.settings.ntp_server = self.ntp_server_edit.clone();
                            let _ = self.ntp.tx.send(ntp::NtpCmd::SetServer(
                                self.settings.ntp_server.clone(),
                            ));
                            let _ = self.ntp.tx.send(ntp::NtpCmd::SyncNow);
                        }
                        let ns = self.ntp.status();
                        let ntp_summary = match ns.offset {
                            Some(off) => format!(
                                "offset={:+.3}s delay={:.1}ms age={}s",
                                off,
                                ns.delay_ms.unwrap_or(0.0),
                                ns.last_sync_age_s.unwrap_or(0)
                            ),
                            None => format!(
                                "no sync: {}",
                                ns.error.as_deref().unwrap_or("connecting")
                            ),
                        };
                        ui.label(ntp_summary);
                    });

                    // PTP settings
                    ui.collapsing("PTP", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Domain:");
                            let old_domain = self.settings.ptp_domain;
                            ui.add(egui::DragValue::new(&mut self.settings.ptp_domain).range(0..=127u8));
                            if self.settings.ptp_domain != old_domain {
                                self.ptp.set_domain(self.settings.ptp_domain);
                            }
                        });
                        let ps = self.ptp.status();
                        let ptp_summary = match ps.offset {
                            Some(off) => format!(
                                "offset={:+.6}s master={} age={}s",
                                off,
                                ps.master.as_deref().unwrap_or("?"),
                                ps.last_sync_age_s.unwrap_or(0)
                            ),
                            None => format!(
                                "no sync: {}",
                                ps.error.as_deref().unwrap_or("no master")
                            ),
                        };
                        ui.label(ptp_summary);
                    });

                    ui.separator();

                    // ── Inputs section ─────────────────────────────────────────────────
                    ui.heading("Inputs");

                    // MTC settings
                    ui.collapsing("MTC (MIDI Timecode)", |ui| {
                        let current_port = self.settings.mtc_port.clone().unwrap_or_default();
                        if self.mtc_selected.is_empty() {
                            self.mtc_selected = current_port.clone();
                        }
                        egui::ComboBox::from_id_salt("mtc_port_combo")
                            .selected_text(if self.mtc_selected.is_empty() {
                                "(none)"
                            } else {
                                &self.mtc_selected
                            })
                            .show_ui(ui, |ui| {
                                for port in &self.mtc_ports.clone() {
                                    ui.selectable_value(
                                        &mut self.mtc_selected,
                                        port.clone(),
                                        port,
                                    );
                                }
                            });
                        ui.horizontal(|ui| {
                            if ui.button("Connect").clicked() && !self.mtc_selected.is_empty() {
                                let _ = self.mtc.connect(&self.mtc_selected.clone());
                                self.settings.mtc_port = Some(self.mtc_selected.clone());
                            }
                            if ui.button("Disconnect").clicked() {
                                self.mtc.disconnect();
                                self.settings.mtc_port = None;
                            }
                        });
                        let ms = self.mtc.status();
                        let mtc_summary = match &ms.error {
                            Some(e) => format!("Error: {}", e),
                            None => match &ms.port {
                                Some(p) => format!("Connected: {}", p),
                                None => "Disconnected".to_string(),
                            },
                        };
                        ui.label(mtc_summary);
                    });

                    // LTC settings
                    ui.collapsing("LTC (Linear Timecode)", |ui| {
                        let current_dev = self.settings.ltc_device.clone().unwrap_or_default();
                        if self.ltc_selected.is_empty() {
                            self.ltc_selected = if current_dev.is_empty() {
                                "(default)".to_string()
                            } else {
                                current_dev
                            };
                        }
                        egui::ComboBox::from_id_salt("ltc_device_combo")
                            .selected_text(&self.ltc_selected)
                            .show_ui(ui, |ui| {
                                for dev in &self.ltc_devices.clone() {
                                    ui.selectable_value(
                                        &mut self.ltc_selected,
                                        dev.clone(),
                                        dev,
                                    );
                                }
                            });
                        ui.horizontal(|ui| {
                            if ui.button("Connect").clicked() {
                                let dev_arg = if self.ltc_selected == "(default)" {
                                    None
                                } else {
                                    Some(self.ltc_selected.as_str())
                                };
                                let _ = self.ltc.connect(dev_arg);
                                self.settings.ltc_device = if self.ltc_selected == "(default)" {
                                    None
                                } else {
                                    Some(self.ltc_selected.clone())
                                };
                            }
                            if ui.button("Disconnect").clicked() {
                                self.ltc.disconnect();
                                self.settings.ltc_device = None;
                            }
                        });
                        let ls = self.ltc.status();
                        let ltc_summary = match &ls.error {
                            Some(e) => format!("Error: {}", e),
                            None => match &ls.device {
                                Some(d) => format!("Connected: {}", d),
                                None => "Disconnected".to_string(),
                            },
                        };
                        ui.label(ltc_summary);
                    });

                    // NTP interface combo
                    ui.horizontal(|ui| {
                        ui.label("NTP interface:");
                        let ntp_text = if self.ntp_nic_selected.is_empty() {
                            auto_label.clone()
                        } else {
                            // Try to find a matching NIC name to show
                            self.nic_list
                                .iter()
                                .find(|(_, ip)| ip.to_string() == self.ntp_nic_selected)
                                .map(|(name, ip)| format!("{name} ({ip})"))
                                .unwrap_or_else(|| self.ntp_nic_selected.clone())
                        };
                        let mut ntp_changed = false;
                        egui::ComboBox::from_id_salt("ntp_nic_combo")
                            .selected_text(&ntp_text)
                            .show_ui(ui, |ui| {
                                // Auto option
                                if ui.selectable_value(
                                    &mut self.ntp_nic_selected,
                                    String::new(),
                                    &auto_label,
                                ).clicked() {
                                    ntp_changed = true;
                                }
                                for (name, ip) in &self.nic_list.clone() {
                                    let label = format!("{name} ({ip})");
                                    let ip_str = ip.to_string();
                                    if ui.selectable_value(
                                        &mut self.ntp_nic_selected,
                                        ip_str,
                                        &label,
                                    ).clicked() {
                                        ntp_changed = true;
                                    }
                                }
                            });
                        if ntp_changed {
                            let bind_ip = if self.ntp_nic_selected.is_empty() {
                                None
                            } else {
                                self.ntp_nic_selected.parse::<Ipv4Addr>().ok()
                            };
                            self.settings.ntp_nic = if self.ntp_nic_selected.is_empty() {
                                None
                            } else {
                                Some(self.ntp_nic_selected.clone())
                            };
                            let _ = self.ntp.tx.send(ntp::NtpCmd::SetBindIp(bind_ip));
                            let _ = self.ntp.tx.send(ntp::NtpCmd::SyncNow);
                        }
                    });

                    // PTP interface combo
                    ui.horizontal(|ui| {
                        ui.label("PTP interface:");
                        let ptp_text = if self.ptp_nic_selected.is_empty() {
                            auto_label.clone()
                        } else {
                            self.nic_list
                                .iter()
                                .find(|(_, ip)| ip.to_string() == self.ptp_nic_selected)
                                .map(|(name, ip)| format!("{name} ({ip})"))
                                .unwrap_or_else(|| self.ptp_nic_selected.clone())
                        };
                        let mut ptp_changed = false;
                        egui::ComboBox::from_id_salt("ptp_nic_combo")
                            .selected_text(&ptp_text)
                            .show_ui(ui, |ui| {
                                if ui.selectable_value(
                                    &mut self.ptp_nic_selected,
                                    String::new(),
                                    &auto_label,
                                ).clicked() {
                                    ptp_changed = true;
                                }
                                for (name, ip) in &self.nic_list.clone() {
                                    let label = format!("{name} ({ip})");
                                    let ip_str = ip.to_string();
                                    if ui.selectable_value(
                                        &mut self.ptp_nic_selected,
                                        ip_str,
                                        &label,
                                    ).clicked() {
                                        ptp_changed = true;
                                    }
                                }
                            });
                        if ptp_changed {
                            let bind_ip = if self.ptp_nic_selected.is_empty() {
                                None
                            } else {
                                self.ptp_nic_selected.parse::<Ipv4Addr>().ok()
                            };
                            self.settings.ptp_nic = if self.ptp_nic_selected.is_empty() {
                                None
                            } else {
                                Some(self.ptp_nic_selected.clone())
                            };
                            self.ptp.set_interface(bind_ip);
                        }
                    });

                    // Refresh NIC list button
                    if ui.button("Refresh NIC list").clicked() {
                        self.refresh_nics();
                    }

                    ui.separator();

                    // Frames display
                    ui.checkbox(&mut self.settings.show_frames, "Show frames (HH:MM:SS:FF)");

                    // Local frame rate (used for System/NTP/PTP when show_frames is on)
                    ui.horizontal(|ui| {
                        ui.label("Local frame rate:");
                        ui.add(
                            egui::DragValue::new(&mut self.settings.local_fps)
                                .speed(0.01)
                                .range(1.0..=120.0)
                                .max_decimals(2),
                        );
                        for &(label, val) in &[
                            ("24", 24.0_f32),
                            ("25", 25.0),
                            ("29.97", 29.97),
                            ("30", 30.0),
                            ("50", 50.0),
                            ("59.94", 59.94),
                            ("60", 60.0),
                        ] {
                            if ui.small_button(label).clicked() {
                                self.settings.local_fps = val;
                            }
                        }
                    });

                    // Text color picker with presets
                    ui.horizontal(|ui| {
                        ui.label("Text color:");
                        ui.color_edit_button_srgb(&mut self.settings.text_color);
                        ui.add_space(6.0);
                        // Preset swatches: Green, White, Amber, Cyan, Red
                        for (label, preset) in &[
                            ("G", [0x00_u8, 0xFF, 0x66]),
                            ("W", [0xF0, 0xF0, 0xF0]),
                            ("A", [0xFF, 0xB3, 0x00]),
                            ("C", [0x00, 0xE5, 0xFF]),
                            ("R", [0xFF, 0x40, 0x40]),
                        ] {
                            let fill = Color32::from_rgb(preset[0], preset[1], preset[2]);
                            if ui.add(
                                egui::Button::new(*label)
                                    .fill(fill)
                                    .min_size(egui::Vec2::splat(18.0)),
                            ).clicked() {
                                self.settings.text_color = *preset;
                            }
                        }
                    });

                    // Font style selector
                    ui.horizontal(|ui| {
                        ui.label("Font:");
                        ui.radio_value(&mut self.settings.font_style, FontStyle::Modern, "Modern");
                        ui.radio_value(&mut self.settings.font_style, FontStyle::SevenSeg, "7-Segment");
                    });

                    ui.separator();

                    // Background opacity slider
                    ui.horizontal(|ui| {
                        ui.label("Background opacity:");
                        ui.add(
                            egui::Slider::new(&mut self.settings.bg_alpha, 0.0..=1.0)
                                .show_value(true),
                        );
                    });

                    // Always on top checkbox
                    let topmost_changed = ui
                        .checkbox(&mut self.settings.topmost, "Always on top")
                        .changed();
                    if topmost_changed {
                        let level = if self.settings.topmost {
                            egui::viewport::WindowLevel::AlwaysOnTop
                        } else {
                            egui::viewport::WindowLevel::Normal
                        };
                        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                    }

                    // Feature A: minimize-on-close checkbox
                    ui.checkbox(
                        &mut self.settings.minimize_on_close,
                        "Minimize to taskbar on close (don't exit)",
                    );
                });
            self.settings_open = open;
        } else {
            // Clear NIC list when settings closed so it refreshes on next open
            self.nic_list.clear();
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([480.0, 320.0])
            .with_min_inner_size([200.0, 140.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_title("StreamClock"),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "stream-clock",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)) as Box<dyn eframe::App>)),
    )
}
