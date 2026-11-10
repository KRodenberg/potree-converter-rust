# Release validation

Version 1.0.0 was validated on Windows x64 on 10 November 2026.

- `cargo test --locked`: 13 tests passed. Coverage includes LAS/LAZ attribute preservation, E57 scan poses and spherical coordinates, LOD point conservation, octant bounds, coincident points, cancellation, failed-project cleanup, OBJ materials/textures, HTML escaping, HTTP ranges and large responses with socket backpressure.
- `cargo clippy --all-targets --locked -- -D warnings` and `cargo fmt --all -- --check`: passed.
- The release desktop window was launched and its rendered settings, file lists, conversion controls and log were visually inspected.
- The final ZIP was extracted into a new folder and its executable launched successfully without adjacent runtime files. Its SHA-256 matched the release build.
- A local E57 survey with 999,945 records converted into 260 octree nodes. An independent decoder checked every output point, node bounds, byte offsets and record lengths against the source record count. No valid points were lost.
- A headless Edge browser loaded the generated project through the executable's local server, decoded point-cloud nodes and rendered the scene without viewer errors or browser exceptions.
- Automatic framing was visually checked for a flat survey and two clouds positioned one kilometre apart.
- PE dependency inspection found Windows system DLLs only; no external converter, LASzip or Visual C++ runtime DLL is required.

Tiny generated LAS, LAZ and E57 fixtures are part of the tests. The real survey and screenshots remain local and are excluded from the public repository and release ZIP. These checks do not establish compatibility with every scanner/vendor extension; supported attributes and omissions are documented in README.md and AGENTS.MD.
