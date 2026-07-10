# Why `StreamClock.entitlements` contains exactly four keys

`macos/StreamClock.entitlements` is the entitlements plist for the **Mac App Store** build
(`cargo build --release --no-default-features --features tc-out`, signed by
`appstore/sign_and_pkg.sh`). It must stay comment-free: `codesign`'s `AMFIUnserializeXML`
parser rejects XML comments anywhere in the file, including inside `<dict>`.

| Key | Why |
|---|---|
| `com.apple.security.app-sandbox` | Required for the Mac App Store. |
| `com.apple.security.network.client` | The optional outbound NTP query. Nothing else. |
| `com.apple.application-identifier` | Must equal `<team>.<bundle id>` and match `Contents/embedded.provisionprofile`, or package validation rejects the upload. |
| `com.apple.developer.team-identifier` | Same reason. |

## What is deliberately absent

**No `com.apple.security.device.audio-input`.** LTC output only *plays* audio. The sandbox has
no "audio-output" entitlement at all — the Hardware entitlement list is
`audio-video-bridging, bluetooth, camera, firewire, audio-input, serial, usb, print`, and
`audio-input` is capture-only. The App Store build never opens a `cpal` input stream, and
`src/ltc.rs` (the only file that calls `input_devices()` / `build_input_stream()`) is compiled
out by `#[cfg(feature = "full-sources")]`.

> Related: `cpal` before 0.17 resolved the *default input device* while opening an output
> stream, which raised a spurious microphone permission prompt (RustAudio/cpal#901, fixed in
> 0.17.0). This crate pins `cpal = "0.18"`. **Do not let that floor drop below 0.17.**

**No mach-lookup temporary exception for CoreMIDI.**
`/System/Library/Sandbox/Profiles/application.sb` (macOS 26.5) already allow-lists

```
(allow mach-lookup
  ...
  (global-name "com.apple.midiserver")
  (global-name "com.apple.midiserver.io")
  (ipc-posix-name-regex "^Apple MIDI (in|out) [0-9]+$"))
```

for every sandboxed process, so `MIDIClientCreate` / `MIDIOutputPortCreate` / `MIDISend` work
with no entitlement. The `com.apple.security.temporary-exception.mach-lookup.global-name =
com.apple.midiserver` folklore dates to macOS 10.7/10.8. **Verified empirically**: the
ad-hoc-sandboxed App Store build sent MTC quarter-frames to a virtual CoreMIDI destination,
with zero sandbox denials in the log. If a future macOS ever regresses this, look for
`deny mach-lookup ... com.apple.midiserver` in Console and add the temporary exception,
justifying it in App Store Connect's "App Sandbox Entitlement Usage Information".

**No `com.apple.security.network.server`** — and this one has a catch.

`network.client` permits outbound flows but **denies `bind(2)` and denies `recv` on an
*unconnected* UDP socket**; both fail with `EPERM`. Measured on macOS 26.5, sandbox +
`network.client` only:

```
socket()                  ok
bind(0.0.0.0:0)           FAIL errno=1 (Operation not permitted)
connect()                 ok
send()  after connect()   ok
recv()  after connect()   ok          ← real NTP reply, stratum=1
sendto() unconnected      ok
recv()  unconnected       FAIL errno=1 (Operation not permitted)
```

So the NTP client must never `bind()`: it creates the socket with `socket2`, `connect()`s to
the server, then `send`/`recv`. See `ntp::unbound_udp_v4`, which is also used by
`main::default_route_ip`. Adding `network.server` would "fix" `bind()` too, but an NTP client
has no business asking for the right to accept incoming connections.
