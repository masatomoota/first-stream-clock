# Vendored winit 0.30.13 — Mac App Store patch

This is an **unmodified copy of `winit` 0.30.13** as published on crates.io, with
**one behavior-neutral change** applied so StreamClock can ship on the Mac App
Store. It is wired in from the app's `Cargo.toml`:

```toml
[patch.crates-io]
winit = { path = "third_party/winit" }
```

`cargo tree -i winit` must show exactly one winit in the graph (0.30.13), so the
patch fully replaces the registry crate.

## Why

App Review rejected StreamClock 1.0.0 (build 1) under **Guideline 2.5.1 —
Performance: Software Requirements** for referencing a non-public API:

```
Contents/MacOS/stream-clock
Symbols:
• _CGSSetWindowBackgroundBlurRadius
```

That symbol is **not** in StreamClock's own source. It comes from winit's macOS
backend, which implements window background blur via the private CoreGraphics
Services functions `CGSSetWindowBackgroundBlurRadius` and `CGSMainConnectionID`
(`WindowDelegate::set_blur`). Apple's static scanner flags any binary that
merely *references* those undefined imports — even though StreamClock never
enables blur (`ViewportBuilder` never calls `.with_blur(true)`, so
`WindowAttributes::blur` stays `false` and the code path is dead at runtime).

Because winit compiles `set_blur` unconditionally on macOS and the branch that
reaches it depends on a runtime value, LTO does not eliminate it — the symbol
stayed in the binary even with `lto = true` + `strip = true`. The only reliable
fix is to stop winit from referencing the symbol, hence this vendored patch.

## The diff (vs. crates.io winit 0.30.13)

1. `src/platform_impl/macos/window_delegate.rs` — `WindowDelegate::set_blur` is
   now a no-op (signature unchanged). It no longer calls the two CGS functions.
2. `src/platform_impl/macos/ffi.rs` — the `extern "C"` declarations for
   `CGSMainConnectionID` and `CGSSetWindowBackgroundBlurRadius` are removed,
   along with the now-unused `use objc2::ffi::NSInteger;` /
   `use objc2::runtime::AnyObject;` imports.

Nothing else is touched. In particular `CGShieldingWindowLevel` (used in the
fullscreen path) is **kept** — it is *public* API declared in
`<CoreGraphics/CGWindowLevel.h>` and is not flagged by App Review.

## Why this is safe

StreamClock never requests window blur, so `set_blur` was already dead code for
this app; making it a no-op changes nothing observable. If a real blurred
background is ever wanted, implement it with public API (`NSVisualEffectView`),
not by restoring the CGS calls.

## Verifying the fix

```sh
# after ./deploy.sh build
nm -u dist/appstore/StreamClock.app/Contents/MacOS/stream-clock | grep -i CGS
#   -> _CGShieldingWindowLevel   (public, OK)
#   (no _CGSSetWindowBackgroundBlurRadius, no _CGSMainConnectionID)
```

## Upgrading winit later

Re-vendor the new version from
`~/.cargo/registry/src/*/winit-<ver>/`, drop `.cargo-ok` / `Cargo.toml.orig` /
`Cargo.lock` / `docs/`, then re-apply the two edits above. Keep this file.
