# Third-party components

The application's original Rust source is MIT-licensed. Embedded and linked third-party components retain their own licenses.

## Viewer runtime

`assets/viewer-libs.zip` packages the existing Potree web runtime and its dependencies. Conversion extracts these files to each generated project's `libs` directory, including existing license and notice files. The portable release also includes copies of the viewer license files under `ThirdPartyLicenses/viewer`.

Major components include Potree (Markus Schütz, BSD-style license), Three.js (MIT), jQuery and jQuery UI (MIT), jstree (MIT), D3 (BSD-style), OpenLayers, proj4js, Spectrum, Tween.js, LAS/LAZ browser loaders and optional Potree support libraries. Consult each bundled library's actual notice for its terms; this list is descriptive rather than a replacement for those notices.

The Rust converter preserves model precision by rewriting a local-coordinate OBJ copy before browser loading. It does not require the legacy OBJ loader's geometry-centering extension. Viewer runtime assets are kept separate from the newly written Rust source so future upgrades can preserve compatibility and all upstream notices.

## Rust dependencies

`Cargo.lock` pins the source dependency graph. Direct dependencies include `eframe`/`egui`, `rfd`, `las`/`laz`, `e57`, `anyhow`, `clap`, `serde`, `serde_json`, `tempfile` and `zip`, together with their transitive dependencies.

The release packaging script collects package metadata and available license/notice text from the exact locked Cargo source graph into `ThirdPartyLicenses/Rust-Dependency-Licenses.txt`. Entries identify package names, versions, SPDX declarations and upstream repositories. Inspect `Cargo.toml` and the collected notices when redistributing a modified release.

No proprietary `e572las.exe`, legacy `PotreeConverter.exe`, LASzip DLL, customer survey or source model is included in this repository or the intended release.

