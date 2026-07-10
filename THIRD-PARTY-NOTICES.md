# Third-Party Notices

StreamClock (this repository, the Windows/macOS desktop app) is distributed under
the [MIT License](LICENSE). It bundles the following third-party components, each
under its own license. Their license terms continue to apply to those components.

## Bundled font

- **DSEG7 Classic (Bold)** — `assets/fonts/DSEG7Classic-Bold.ttf`, embedded in the
  binary via `include_bytes!`.
  Copyright (c) 2017, keshikan (http://www.keshikan.net), with Reserved Font Name
  "DSEG". Licensed under the **SIL Open Font License, Version 1.1**.
  Full license text: [`assets/fonts/DSEG-LICENSE.txt`](assets/fonts/DSEG-LICENSE.txt).
  Per the OFL: this font is provided as-is, may not be sold by itself, and the
  Reserved Font Name "DSEG" may not be used for modified versions. The license
  text above must accompany any distribution that includes this font.

## Rust dependencies (direct)

Resolved transitively into the binary; see `Cargo.toml` / `Cargo.lock` for exact
versions. A complete transitive list with license texts can be generated with
`cargo about generate` or `cargo license`.

| Crate     | License             |
|-----------|---------------------|
| eframe / egui | MIT OR Apache-2.0 |
| chrono    | MIT OR Apache-2.0   |
| serde     | MIT OR Apache-2.0   |
| if-addrs  | MIT OR Apache-2.0   |
| midir     | MIT                 |
| cpal      | Apache-2.0          |

`midir` and `cpal` are only compiled into the **full** build (Cargo feature
`full-sources`, for MTC/LTC). The **App Store / lite** build
(`--no-default-features`) excludes them.

All of the above are permissive licenses compatible with MIT distribution and with
Apple App Store distribution. Retain the copyright notices when redistributing.
