# Potree Converter for Windows

A portable Rust desktop application that converts **LAS, LAZ and E57** files into Potree 2 point clouds and a local web viewer. The layout follows the original PowerShell LAZ converter, with a cleaner native window, batch inputs, project details, optional OBJ models, camera controls, progress and a conversion log.

The release executable contains the conversion engine and viewer assets. It does not require PowerShell, Python, PDAL, LASzip DLLs, the original PotreeConverter executable or a Rust installation. A browser and Windows graphics drivers are needed to display the desktop window and generated viewer.

## Use

1. Extract the Windows ZIP and open `potree-converter.exe`.
2. Add or drag in one or more `.las`, `.laz` or `.e57` files.
3. Set the project/job number, viewer title and output folder.
4. Optionally add OBJ files and adjust model translation, camera, lighting or transparency.
5. Start conversion. Each input becomes a separate cloud in the same viewer.
6. Use **Open viewer** when conversion finishes. Keep the converter open while using this local preview.

The optional viewer password is a browser convenience screen. Its value and all point-cloud data are downloadable by anyone who can access the project files. Use authenticated hosting when access control is required.

The generated project contains its own `libs` folder; it can be copied to a web host without a shared library installation. Browsers need HTTP to load the point-cloud files; opening the HTML using `file://` is insufficient.

```text
Output/
  JOB-001/
    index.html
    libs/
    pointclouddata/
      cloud-name/
        metadata.json
        hierarchy.bin
        octree.bin
      models/
```

## Command line

```powershell
.\potree-converter.exe convert --input scan.laz scan.e57 --output C:\Output --job JOB-001 --title "Site survey"
.\potree-converter.exe --serve C:\Output\JOB-001 --port 8080
.\potree-converter.exe --help
```

Conversion options include `--obj model.obj`, `--password "optional value"`, `--translation 0 0 0`, `--camera-position 1 2 3`, `--camera-target 0 0 0`, `--no-lights`, `--keep-transparency` and `--node-points 20000`. Camera position and target must be supplied together. Launch without arguments for the GUI.

The preview server listens only on `127.0.0.1`. It supports byte ranges used by Potree, serves only files within the selected project and does not list directories. Stop the command-line preview with Ctrl+C.

## Models and coordinates

OBJ models are optional. The converter packages referenced local materials and textures and leaves source files untouched. It streams a rewritten OBJ copy with its bounds centre subtracted in Rust double precision, before the browser creates Float32 geometry. The viewer rotates conventional OBJ Y-up coordinates into Potree Z-up and applies the entered Easting/Northing/RL translation. Start with `0, 0, 0` and use the model bounds report when alignment is needed. Large world coordinates can reduce browser rendering precision.

Point clouds keep their source coordinate system. E57 scan poses are applied during reading. The converter does not reproject between coordinate reference systems: inputs must already share a coordinate system to align correctly. Available LAS WKT/E57 coordinate metadata is copied; GeoTIFF-key-only CRS definitions are not interpreted. Point coordinates use a scale of at least `0.000001` source units, increasing as needed to fit signed 32-bit integers.

LAS RGB, intensity, classification code, return number/count, source ID and GPS time are preserved when available. E57 color/intensity are normalized, and source ID identifies the scan. LAS extra bytes, NIR, waveform data, classification/scanner flags, scan angle/user data, and E57 images, row/column structure and vendor metadata are not exported. A node target is a tuning value: a leaf at the maximum octree depth of 24 can contain more points when data is highly concentrated.

Conversion streams point records through temporary disk storage; the small hierarchy and work queue grow with node count. Budget conservatively for roughly 90 bytes per current input point of scratch space plus 33 bytes per output point, hierarchy overhead and about 57 MB of viewer libraries. For multiple inputs, already converted clouds also occupy staging space. Input files require their own storage. LAZ compression does not carry over to the Potree output. Failed/cancelled jobs are cleaned up before publication, and existing project folders are preserved. Cancellation is checked during point reading and octree construction; model/viewer packaging finishes its current step first.

## Build and package

Install stable Rust for `x86_64-pc-windows-msvc` and Microsoft C++ Build Tools with the Windows SDK, then run:

```powershell
cargo build --release --locked
cargo test --locked
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\package.ps1
```

The package script builds the release, collects dependency notices, and writes the portable ZIP and SHA-256 checksum under `dist`. `.cargo/config.toml` statically links the Microsoft C runtime. Normal Windows system DLLs remain operating-system dependencies.

`assets/viewer-libs.zip` is the embedded Potree/Three.js runtime, including existing library notices. No customer surveys, generated project outputs or proprietary legacy conversion executables belong in this repository.

See [AGENTS.MD](AGENTS.MD) for the implementation and reconstruction guide, and [THIRD_PARTY.md](THIRD_PARTY.md) for bundled components.

