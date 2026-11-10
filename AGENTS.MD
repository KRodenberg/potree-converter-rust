# Reconstruction and maintenance guide

## Purpose

Recreate the legacy PowerShell LAZ-to-Potree desktop workflow as a self-contained Windows Rust executable, extending input support to LAS and E57. The executable embeds both a native conversion engine and the web viewer runtime. It must not shell out to the old PotreeConverter, e572las, Python, PDAL or an installed JavaScript runtime.

The original application used WinForms and a vertical sequence of input files, job/title/output, optional password, model translation, optional camera override, lighting, optional OBJ models, transparency, progress/log and Convert. Keep this conceptual arrangement understandable when changing the GUI.

## Repository map and contracts

- `src/main.rs`: process entry point and command-line parsing. No arguments opens the GUI. `convert` runs a conversion. `--serve <project> --port <port>` runs the preview server. Release builds use the Windows GUI subsystem; CLI invocations attach to the parent console before parsing arguments so terminal output remains visible.
- `src/gui.rs`: egui/eframe desktop window, file dialogs and drag/drop, request editing, worker lifecycle and progress/log messages. Four settings cards cover input files, project/viewer, OBJ placement, and camera/conversion. A footer exposes conversion, cancellation, preview and output-folder actions. Defaults are an empty password, translation `0,0,0`, automatic camera, enabled lights/transparency flip and 20,000 target points per node. The default output directory is `PotreeOutput` beside the executable.
- `src/job.rs`: validation, safe Windows names, unique per-input cloud directories, progress aggregation and staging/publication. Interface: `run(&ConversionRequest, &AtomicBool, &dyn Fn(Progress)) -> Result<PathBuf>`. `validate` is shared by the GUI and conversion entry point; `Progress` carries log lines or fraction/status updates.
- `src/model.rs`: shared `ConversionRequest` and `Camera` data. The request contains input/model paths, output directory, project/job, title, optional password, translation, optional camera, lighting/transparency choices and node point target.
- `src/cloud.rs` and `src/cloud/`: native point-cloud reading, disk spooling, octree construction and Potree serialization. Public conversion interface: `convert(input, output, node_points, cancel, report) -> Result<CloudInfo>`. `CloudInfo` reports accepted point count and world bounds; `CloudProgress` reports a fraction and human-readable message.
- `src/viewer.rs` and optional `src/viewer/`: material/texture packaging, embedded library extraction and HTML generation. Interface: `write_viewer(project, request, clouds, log) -> Result<PathBuf>`. Each cloud tuple contains a display name and a project-relative metadata path.
- `src/server.rs`: loopback HTTP preview. `start(root: PathBuf, port: u16) -> Result<PreviewServer>` returns a URL. Port zero selects a free port. Keep the returned object alive; dropping it stops accepting requests.
- `assets/viewer.html`: neutral standalone viewer template, serialized configuration, visible cloud/model loading failures, aggregate scene framing, optional camera override, model lighting and optional browser password screen.
- `assets/viewer-libs.zip`: distributable viewer libraries and their notices, embedded with `include_bytes!`. OBJ centering is performed by Rust before browser loading; this implementation does not call or require the legacy loader's `setCenterGeometry(true)` extension.
- `.cargo/config.toml`: `+crt-static` for Windows MSVC.
- `scripts/package.ps1`: release packaging, dependency notices and SHA-256 hashes.
- `.github/workflows/windows.yml`: Windows build, tests and downloadable portable artifact.

`src/lib.rs` exposes the reusable conversion, viewer and preview modules. `src/cloud/tests.rs` contains generated-fixture conversion tests. Read the current entry point before changing a contract.

## Point-cloud conversion

Use the pure Rust `las` crate with its `laz` feature for LAS and compressed LAZ. Use the pure Rust `e57` crate for E57, including every scan, spherical-to-Cartesian coordinates, normalized color/intensity and each scan's rigid pose. Non-finite coordinates and E57 invalid-coordinate records are skipped and counted before bounds calculation. Structural decoding errors fail the job with input/record context. Empty input is an error. Non-finite GPS time becomes zero.

Decode points sequentially into **45-byte** little-endian temporary records: XYZ `f64[3]` at bytes 0–23, RGB `u16[3]` at 24–29, intensity `u16` at 30–31, classification/return number/return count `u8` at 32–34, source ID `u16` at 35–36 and GPS time `f64` at 37–44. Compute aggregate bounds while decoding, then process oversized octree nodes breadth-first using disk partitions.

At each internal node, select the first point in each occupied regular-grid cell; the grid side is `floor(cuberoot(node_points))`, at least two. Send remaining points to children. Child index is `4*x_half + 2*y_half + z_half`, and corresponding child-mask bits use that index. Parent samples and descendant points are disjoint, preserving the exact accepted point count. Subdivision stops at depth **24**. A terminal leaf at that depth can exceed the target node count for coincident or highly concentrated data; fail if a leaf cannot fit its `u32` count.

Point buffers and cell occupancy are bounded by the node target; the in-memory node metadata and breadth-first work queue grow with node count. Cancellation is checked every 4,096 decoded/partitioned records and between cloud/job stages. Viewer/model packaging completes its current step before the job-level cancellation check.

Budget conservatively for up to roughly **90 bytes per current input point of scratch space**, **33 bytes per output point**, hierarchy overhead and approximately 57 MB of uncompressed viewer assets. Previously completed input clouds also remain in staging. Actual scratch usage falls as parent spools are deleted. Source input storage is additional; compressed LAZ size is not a useful estimate of required conversion space.

Potree 2 output has exactly three required cloud files:

1. `metadata.json` describes version `2.0`, point count, cubic bounds, quantization scale/offset, spacing, attribute layouts and hierarchy parameters.
2. `hierarchy.bin` stores nodes in breadth-first order. Every standard entry is **22 bytes**: node type `u8`, child mask `u8`, point count little-endian `u32`, byte offset little-endian `u64`, byte size little-endian `u64`. Child bits use the octree X/Y/Z ordering expected by Potree's loader. A small/full hierarchy can use one first hierarchy chunk without proxy entries.
3. `octree.bin` stores **33-byte interleaved** point records at each node's advertised offset: quantized XYZ `i32[3]` at bytes 0–11; RGB `u16[3]` at 12–17; intensity `u16` at 18–19; classification, return number and return count `u8` at 20–22; source ID `u16` at 23–24; GPS time `f64` at 25–32. `DEFAULT` encoding uses the same layout for every node, declared exactly by `metadata.json`.

Preserve LAS RGB, intensity, classification code, return number/count, point source ID and GPS time where present. Missing GPS time is zero; missing color uses an intensity-derived/default grey. E57 RGB/intensity are normalized into `u16`; source ID is the zero-based scan index saturated at 65,535, while unavailable LAS-style classification/return/GPS fields are zero. LAS extra bytes, NIR, waveform records, classification/scanner flags, scan angle and user data are omitted. E57 images, row/column structure and vendor-specific metadata are omitted. Record these limitations in metadata and user documentation.

The embedded `libs/potree/workers/2.0/DecoderWorker.js` and Potree `OctreeLoader` are the compatibility references. Position reconstruction follows `integer * scale + offset` before node-relative subtraction. Test ranges, signs, child masks and attribute offsets; a metadata file that parses does not prove a correct cloud.

Keep geometry in its original CRS; this program does not perform CRS reprojection. Copy LAS `LASF_Projection` WKT record 2112 or E57 coordinate metadata into the projection string when available; otherwise leave it empty. GeoTIFF-key-only CRS records are not interpreted. Browser rendering must still work in source coordinates if the viewer cannot interpret a WKT string; do not invent a CRS or transform coordinates as a fallback.

The global offset is the accepted XYZ minimum. Start with a cubic side equal to the largest extent, at least `0.001` source units and at least `max_coordinate_magnitude * f64::EPSILON * 8`. Quantization scale is `max(side / (i32::MAX - 2), 0.000001)` source units, then increase cube side by one scale unit. Round `(coordinate - offset) / scale` to signed `i32`, explicitly checking representability. This gives finite positive scale/spacing for degenerate bounds and keeps quantization error within about half a scale unit.

## Project publication and GUI threading

Run conversion on a worker thread; update the GUI through an `mpsc` channel. Never decode large surveys on the UI thread. The UI remains responsive, disables settings while busy and exposes cancellation. Closing during conversion requests cancellation and waits for worker completion. Catch unexpected worker panics and present an error. Bound the visible log; it is not an unbounded survey record store. Validate missing inputs, supported extensions, nonempty job/title, node target (100–1,000,000 in the job API), finite camera/translation values, distinct camera position/target and the output location before work begins. Duplicate cloud basenames receive unique output suffixes.

Build each complete project in `.potree-building-*` temporary storage under the selected output directory. Each cloud's `.octree-work-*` temporary directory holds source/partition spools. Publish by renaming the completed staged project to `<output>/<safe-job>` only after every input, model and viewer succeeds and cancellation is checked. Check for an existing destination both before starting and before publishing. Temporary-directory guards clean failed/cancelled staging; source files are never edited.

Use collision-safe relative cloud/model names. Avoid deriving shell commands from project titles or file paths. Input extensions should be case-insensitive. One cloud directory per source prevents metadata files from overwriting one another.

## Viewer, OBJ and materials

Extract embedded libraries safely using enclosed ZIP paths. Preserve all notices. Serialize viewer settings as JSON rather than concatenating JavaScript strings; escape `<`, `>` and `&` in embedded JSON to prevent a title or path from closing a script tag. HTML-visible text also needs escaping.

Load every selected cloud and display loading failures. By default frame the aggregate loaded geometry; an explicit camera position and world target override this. Apply optional directional/ambient model lighting without changing source point colors.

Scan OBJ vertex lines incrementally for bounds, then stream a rewritten **copy** whose vertex XYZ values have the source centre subtracted in Rust `f64`. Preserve optional vertex tail data. Browser OBJ loading therefore receives local coordinates before creating Float32 geometry, with no custom centering-loader dependency. Rotate the loaded object +90 degrees about X into Z-up, then add the user's Easting/Northing/RL translation. Per-model centres differ even when a shared translation is used. The source centre `(X,Y,Z)` maps to `(X,-Z,Y)` when restoring source placement; report bounds and that centre in the log.

Copy OBJ, MTL and local texture dependencies into the project and rewrite copied references. Resolve MTL-relative texture paths, quoted names, filenames with spaces and supported MTL options. Report missing textures. The transparency option rewrites `Tr v` to `Tr (1-v)` in copied materials. Never edit source models or source MTLs.

An optional viewer password is only a client-side convenience gate. It is visible to anyone downloading the files. Never describe it as encryption or server authentication.

## Preview server

Bind only `127.0.0.1`, not all interfaces. Support GET, HEAD and single byte ranges, including suffix ranges. Stream file slices; never read `octree.bin` into RAM. Send `Content-Length`, `Content-Range` for partial requests, and `Accept-Ranges: bytes`. Invalid ranges return 416.

Percent-decode URL paths, reject traversal and Windows alternate stream/absolute-path syntax, canonicalize final paths, and require containment within the canonical project root. Canonicalization also prevents symlinks from escaping that root. `/` serves `index.html` or the first top-level HTML viewer. Do not provide directory listings. Bound header sizes, socket timeouts and concurrent requests.

## Build, validation and delivery

Target Windows x64 using stable Rust with Microsoft C++ Build Tools and the Windows SDK. Build with `cargo build --release --locked`. Test with `cargo test --locked`, format using `cargo fmt --all -- --check`, and lint as practical. `.cargo/config.toml` adds `-C target-feature=+crt-static`; normal Windows system DLL imports are expected, external converter/runtime DLL imports are not.

Meaningful regression tests cover LAS and LAZ decoding, E57 transforms and scan iteration, hierarchy record counts/offsets, LOD point conservation, degenerate clouds, malformed inputs, cancellation, OBJ material dependencies, HTML escaping, preview byte ranges and path confinement. Use tiny generated fixtures in tests. Real customer fixtures may be used locally but must never be committed or included in release archives.

Run `scripts/package.ps1` to produce `dist/Potree-Converter-1.0.0-windows-x64.zip` and checksum. The ZIP contains the standalone executable, user/developer documentation, license notices and the executable hash. Verify a clean-folder launch and an end-to-end generated viewer before claiming a release is complete. Record what was actually tested; do not claim every vendor variant of LAS/E57 has been validated.

The public repository is `https://github.com/KRodenberg/potree-converter-rust`. Publish source, lockfile, embedded redistributable viewer assets and documentation. Exclude `target`, `dist`, customer point clouds, OBJ assets, logs, secrets and generated projects. A binary may be distributed separately as a release artifact.

