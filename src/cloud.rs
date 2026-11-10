//! Native, streaming input decoding and disk-backed Potree 2 octree construction.
//!
//! Potree DEFAULT records are interleaved. Hierarchy entries are breadth-first,
//! 22 bytes: type:u8, children:u8, count:u32, offset:u64, size:u64 (little endian).
use anyhow::{bail, ensure, Context, Result};
use serde_json::json;
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const SPOOL_BYTES: usize = 45;
const OUTPUT_BYTES: usize = 33;
const MAX_DEPTH: usize = 24;

pub struct CloudProgress {
    pub fraction: f32,
    pub message: String,
}

#[derive(Debug)]
pub struct CloudInfo {
    pub points: u64,
    pub bounds: [[f64; 3]; 2],
}

#[derive(Clone, Debug, Default)]
struct Point {
    xyz: [f64; 3],
    rgb: [u16; 3],
    intensity: u16,
    classification: u8,
    return_number: u8,
    number_of_returns: u8,
    source: u16,
    gps: f64,
}

impl Point {
    fn packed(&self) -> [u8; SPOOL_BYTES] {
        let mut b = [0; SPOOL_BYTES];
        for i in 0..3 {
            b[i * 8..i * 8 + 8].copy_from_slice(&self.xyz[i].to_le_bytes());
            b[24 + i * 2..26 + i * 2].copy_from_slice(&self.rgb[i].to_le_bytes());
        }
        b[30..32].copy_from_slice(&self.intensity.to_le_bytes());
        b[32] = self.classification;
        b[33] = self.return_number;
        b[34] = self.number_of_returns;
        b[35..37].copy_from_slice(&self.source.to_le_bytes());
        b[37..45].copy_from_slice(&self.gps.to_le_bytes());
        b
    }

    fn unpack(b: &[u8; SPOOL_BYTES]) -> Self {
        Self {
            xyz: std::array::from_fn(|i| {
                f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap())
            }),
            rgb: std::array::from_fn(|i| {
                u16::from_le_bytes(b[24 + i * 2..26 + i * 2].try_into().unwrap())
            }),
            intensity: u16::from_le_bytes(b[30..32].try_into().unwrap()),
            classification: b[32],
            return_number: b[33],
            number_of_returns: b[34],
            source: u16::from_le_bytes(b[35..37].try_into().unwrap()),
            gps: f64::from_le_bytes(b[37..45].try_into().unwrap()),
        }
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Conversion cancelled");
    Ok(())
}

fn progress(report: &dyn Fn(CloudProgress), fraction: f32, message: impl Into<String>) {
    report(CloudProgress {
        fraction,
        message: message.into(),
    });
}

struct Spool {
    writer: BufWriter<File>,
    points: u64,
    seen: u64,
    skipped: u64,
    min: [f64; 3],
    max: [f64; 3],
    gps_min: f64,
    gps_max: f64,
}

impl Spool {
    fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            writer: BufWriter::with_capacity(1024 * 1024, File::create(path)?),
            points: 0,
            seen: 0,
            skipped: 0,
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
            gps_min: f64::INFINITY,
            gps_max: f64::NEG_INFINITY,
        })
    }

    fn add(&mut self, mut p: Point) -> Result<()> {
        self.seen += 1;
        if !p.xyz.iter().all(|v| v.is_finite()) {
            self.skipped += 1;
            return Ok(());
        }
        if !p.gps.is_finite() {
            p.gps = 0.0;
        }
        for i in 0..3 {
            self.min[i] = self.min[i].min(p.xyz[i]);
            self.max[i] = self.max[i].max(p.xyz[i]);
        }
        self.gps_min = self.gps_min.min(p.gps);
        self.gps_max = self.gps_max.max(p.gps);
        self.writer.write_all(&p.packed())?;
        self.points += 1;
        Ok(())
    }
}

fn read_las(
    input: &Path,
    spool: &mut Spool,
    cancel: &AtomicBool,
    report: &dyn Fn(CloudProgress),
) -> Result<String> {
    let mut reader = las::Reader::from_path(input).context("Opening LAS/LAZ input")?;
    let total = reader.header().number_of_points().max(1);
    let projection = reader
        .header()
        .all_vlrs()
        .find(|vlr| vlr.user_id == "LASF_Projection" && vlr.record_id == 2112)
        .map(|vlr| {
            String::from_utf8_lossy(&vlr.data)
                .trim_end_matches('\0')
                .to_owned()
        })
        .unwrap_or_default();
    for (index, item) in reader.points().enumerate() {
        if index % 4096 == 0 {
            cancelled(cancel)?;
        }
        let p = item.with_context(|| format!("Reading LAS/LAZ point {}", index + 1))?;
        let grey = match p.intensity {
            0 => 200,
            1..=255 => p.intensity,
            value => value / 256,
        };
        let rgb = p
            .color
            .map(|c| [c.red, c.green, c.blue])
            .unwrap_or([grey; 3]);
        spool.add(Point {
            xyz: [p.x, p.y, p.z],
            rgb,
            intensity: p.intensity,
            classification: p.classification.into(),
            return_number: p.return_number,
            number_of_returns: p.number_of_returns,
            source: p.point_source_id,
            gps: p.gps_time.unwrap_or(0.0),
        })?;
        if index % 131072 == 0 {
            progress(
                report,
                0.45 * (index as f32 / total as f32).min(1.0),
                format!("Reading LAS/LAZ: {} points", index + 1),
            );
        }
    }
    Ok(projection)
}

fn normalized_u16(v: f32) -> u16 {
    if v.is_finite() {
        (v.clamp(0.0, 1.0) * 65535.0).round() as u16
    } else {
        0
    }
}

fn read_e57(
    input: &Path,
    spool: &mut Spool,
    cancel: &AtomicBool,
    report: &dyn Fn(CloudProgress),
) -> Result<String> {
    let mut reader = e57::E57Reader::from_file(input).context("Opening E57 input")?;
    let projection = reader.coordinate_metadata().unwrap_or("").to_owned();
    let scans = reader.pointclouds();
    ensure!(!scans.is_empty(), "E57 contains no point-cloud scans");
    let total = scans.iter().map(|s| s.records).sum::<u64>().max(1);
    let mut seen = 0u64;
    for (scan_index, scan) in scans.iter().enumerate() {
        cancelled(cancel)?;
        progress(
            report,
            0.45 * seen as f32 / total as f32,
            format!("Reading E57 scan {} of {}", scan_index + 1, scans.len()),
        );
        // The simple reader normalizes RGB/intensity, converts spherical points,
        // and applies each scan's rigid pose to put it in the common file frame.
        let mut points = reader.pointcloud_simple(scan)?;
        points.apply_pose(true);
        points.spherical_to_cartesian(true);
        points.intensity_to_color(true);
        let mut empty_packets = 0usize;
        loop {
            if seen.is_multiple_of(4096) || empty_packets > 0 {
                cancelled(cancel)?;
            }
            let point = match points.next() {
                None => break,
                Some(Err(e57::Error::Internal { ref desc, .. }))
                    if desc == "Cannot read next point because of logic error" =>
                {
                    // e57 0.11.13's simple iterator advances one packet at a
                    // time and returns this error when an otherwise valid
                    // packet has no complete point yet (e.g. partial integer
                    // bitstreams). The raw iterator instead keeps advancing.
                    // Retrying only this exact error does the same safely:
                    // each retry consumes a packet; normal parse/I/O errors
                    // still propagate. Bound retries for malformed inputs.
                    empty_packets += 1;
                    ensure!(
                        empty_packets <= 65536,
                        "E57 contains too many packets without complete point records"
                    );
                    continue;
                }
                Some(point) => point,
            };
            empty_packets = 0;
            seen += 1;
            let p = point
                .with_context(|| format!("Reading E57 scan {}, point {}", scan_index + 1, seen))?;
            if let e57::CartesianCoordinate::Valid { x, y, z } = p.cartesian {
                let rgb = p
                    .color
                    .map(|c| {
                        [
                            normalized_u16(c.red),
                            normalized_u16(c.green),
                            normalized_u16(c.blue),
                        ]
                    })
                    .unwrap_or([200 * 257; 3]);
                spool.add(Point {
                    xyz: [x, y, z],
                    rgb,
                    intensity: p.intensity.map(normalized_u16).unwrap_or(0),
                    source: u16::try_from(scan_index).unwrap_or(u16::MAX),
                    ..Point::default()
                })?;
            } else {
                spool.seen += 1;
                spool.skipped += 1;
            }
            if seen.is_multiple_of(131072) {
                progress(
                    report,
                    0.45 * seen as f32 / total as f32,
                    format!("Reading E57: {seen} records"),
                );
            }
        }
    }
    Ok(projection)
}

#[derive(Clone)]
struct Pending {
    id: usize,
    path: PathBuf,
    count: u64,
    min: [f64; 3],
    side: f64,
    depth: usize,
}

#[derive(Default)]
struct Node {
    count: u32,
    mask: u8,
    offset: u64,
    bytes: u64,
}

fn output_point(writer: &mut impl Write, point: &Point, min: [f64; 3], scale: f64) -> Result<()> {
    let mut b = [0; OUTPUT_BYTES];
    for i in 0..3 {
        let value = ((point.xyz[i] - min[i]) / scale).round();
        ensure!(
            value.is_finite() && value >= 0.0 && value <= i32::MAX as f64,
            "Coordinate exceeds Potree int32 quantization range"
        );
        b[i * 4..i * 4 + 4].copy_from_slice(&(value as i32).to_le_bytes());
    }
    let packed = point.packed();
    b[12..].copy_from_slice(&packed[24..]);
    writer.write_all(&b)?;
    Ok(())
}

/// Convert one source file. Point buffers are bounded by the node limit; the
/// small hierarchy and work queue grow with node count. Temporary point storage
/// lives under `output` and is removed on return.
pub fn convert(
    input: &Path,
    output: &Path,
    node_points: usize,
    cancel: &AtomicBool,
    report: &dyn Fn(CloudProgress),
) -> Result<CloudInfo> {
    cancelled(cancel)?;
    ensure!(
        (8..=1_000_000).contains(&node_points),
        "Points per node must be between 8 and 1,000,000"
    );
    fs::create_dir_all(output)?;
    let temp = tempfile::Builder::new()
        .prefix(".octree-work-")
        .tempdir_in(output)?;
    let source = temp.path().join("source.bin");
    let mut spool = Spool::new(&source)?;
    progress(report, 0.0, "Reading source points");
    let extension = input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let projection = match extension.as_str() {
        "las" | "laz" => read_las(input, &mut spool, cancel, report)?,
        "e57" => read_e57(input, &mut spool, cancel, report)?,
        _ => bail!("Unsupported file extension: {extension}; choose LAS, LAZ or E57"),
    };
    spool.writer.flush()?;
    drop(spool.writer);
    ensure!(spool.points > 0, "Source contains no valid finite points");
    if spool.skipped > 0 {
        progress(
            report,
            0.45,
            format!("Skipped {} invalid/nonfinite point records", spool.skipped),
        );
    }
    let bounds = [spool.min, spool.max];
    let mut side = (0..3)
        .map(|i| spool.max[i] - spool.min[i])
        .fold(0.001_f64, f64::max);
    let magnitude = spool
        .min
        .iter()
        .chain(spool.max.iter())
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    side = side.max(magnitude * f64::EPSILON * 8.0);
    ensure!(
        side.is_finite(),
        "Source coordinate extent is not representable"
    );
    let scale = (side / (i32::MAX as f64 - 2.0)).max(0.000001);
    // Leave half a quantization unit of space at the upper boundary.
    side += scale;
    let cube_max: [f64; 3] = std::array::from_fn(|i| spool.min[i] + side);
    ensure!(
        cube_max.iter().all(|v| v.is_finite()),
        "Source bounding box overflows"
    );
    let mut nodes = vec![Node::default()];
    let mut queue = VecDeque::from([Pending {
        id: 0,
        path: source,
        count: spool.points,
        min: spool.min,
        side,
        depth: 0,
    }]);
    let mut octree =
        BufWriter::with_capacity(1024 * 1024, File::create(output.join("octree.bin"))?);
    let mut offset = 0u64;
    let mut written = 0u64;
    let mut deepest = 0usize;
    let grid = (node_points as f64).cbrt().floor().max(2.0) as usize;
    while let Some(pending) = queue.pop_front() {
        cancelled(cancel)?;
        let all = pending.count <= node_points as u64 || pending.depth >= MAX_DEPTH;
        let mut occupied = if all {
            Vec::new()
        } else {
            vec![false; grid * grid * grid]
        };
        let mut child_writers: [Option<BufWriter<File>>; 8] = std::array::from_fn(|_| None);
        let child_paths: [PathBuf; 8] =
            std::array::from_fn(|i| temp.path().join(format!("{}-{i}.bin", pending.id)));
        let mut child_counts = [0u64; 8];
        let mut reader = BufReader::with_capacity(1024 * 1024, File::open(&pending.path)?);
        let start = offset;
        let mut selected = 0u64;
        for i in 0..pending.count {
            if i % 4096 == 0 {
                cancelled(cancel)?;
            }
            let mut raw = [0; SPOOL_BYTES];
            reader
                .read_exact(&mut raw)
                .context("Reading temporary point spool")?;
            let point = Point::unpack(&raw);
            let local: [f64; 3] = std::array::from_fn(|axis| {
                ((point.xyz[axis] - pending.min[axis]) / pending.side).clamp(0.0, 1.0)
            });
            let keep = if all {
                true
            } else {
                let cell: [usize; 3] =
                    std::array::from_fn(|a| ((local[a] * grid as f64) as usize).min(grid - 1));
                let index = cell[0] + grid * (cell[1] + grid * cell[2]);
                let keep = !occupied[index];
                occupied[index] = true;
                keep
            };
            if keep {
                output_point(&mut octree, &point, spool.min, scale)?;
                offset += OUTPUT_BYTES as u64;
                selected += 1;
            } else {
                let child = ((local[0] >= 0.5) as usize) * 4
                    + ((local[1] >= 0.5) as usize) * 2
                    + (local[2] >= 0.5) as usize;
                if child_writers[child].is_none() {
                    child_writers[child] = Some(BufWriter::with_capacity(
                        64 * 1024,
                        File::create(&child_paths[child])?,
                    ));
                }
                child_writers[child].as_mut().unwrap().write_all(&raw)?;
                child_counts[child] += 1;
            }
        }
        drop(reader);
        fs::remove_file(&pending.path)?;
        let mut mask = 0u8;
        for child in 0..8 {
            if let Some(mut writer) = child_writers[child].take() {
                writer.flush()?;
            }
            if child_counts[child] == 0 {
                continue;
            }
            mask |= 1 << child;
            let child_id = nodes.len();
            nodes.push(Node::default());
            let child_side = pending.side / 2.0;
            let child_min = std::array::from_fn(|axis| {
                pending.min[axis]
                    + if child & (4 >> axis) != 0 {
                        child_side
                    } else {
                        0.0
                    }
            });
            queue.push_back(Pending {
                id: child_id,
                path: child_paths[child].clone(),
                count: child_counts[child],
                min: child_min,
                side: child_side,
                depth: pending.depth + 1,
            });
        }
        nodes[pending.id] = Node {
            count: u32::try_from(selected)
                .context("A terminal octree node exceeds 4 billion points")?,
            mask,
            offset: start,
            bytes: offset - start,
        };
        written += selected;
        deepest = deepest.max(pending.depth);
        progress(
            report,
            0.45 + 0.5 * written as f32 / spool.points as f32,
            format!(
                "Building octree: {written}/{} points, {} nodes",
                spool.points,
                nodes.len()
            ),
        );
    }
    octree.flush()?;
    ensure!(
        written == spool.points,
        "Internal error: octree did not preserve every valid point"
    );
    cancelled(cancel)?;
    let mut hierarchy = BufWriter::new(File::create(output.join("hierarchy.bin"))?);
    for node in &nodes {
        hierarchy.write_all(&[if node.mask == 0 { 1 } else { 0 }, node.mask])?;
        hierarchy.write_all(&node.count.to_le_bytes())?;
        hierarchy.write_all(&node.offset.to_le_bytes())?;
        hierarchy.write_all(&node.bytes.to_le_bytes())?;
    }
    hierarchy.flush()?;
    let attribute = |name: &str,
                     kind: &str,
                     elements: usize,
                     element_size: usize,
                     min: Vec<f64>,
                     max: Vec<f64>| {
        json!({
            "name":name,"description":"","size":elements*element_size,"numElements":elements,
            "elementSize":element_size,"type":kind,"min":min,"max":max
        })
    };
    let attrs = vec![
        attribute(
            "position",
            "int32",
            3,
            4,
            spool.min.to_vec(),
            spool.max.to_vec(),
        ),
        attribute("rgb", "uint16", 3, 2, vec![0.; 3], vec![65535.; 3]),
        attribute("intensity", "uint16", 1, 2, vec![0.], vec![65535.]),
        attribute("classification", "uint8", 1, 1, vec![0.], vec![255.]),
        attribute("return number", "uint8", 1, 1, vec![0.], vec![15.]),
        attribute("number of returns", "uint8", 1, 1, vec![0.], vec![15.]),
        attribute("source id", "uint16", 1, 2, vec![0.], vec![65535.]),
        attribute(
            "gps-time",
            "double",
            1,
            8,
            vec![spool.gps_min],
            vec![spool.gps_max],
        ),
    ];
    let metadata = json!({
        "version":"2.0", "name":input.file_stem().and_then(|s|s.to_str()).unwrap_or("cloud"),
        "description":"Converted by Potree Converter Rust", "points":spool.points,
        "projection":projection,"hierarchy":{"firstChunkSize":nodes.len()*22,"stepSize":deepest+1,"depth":deepest},
        "offset":spool.min,"scale":[scale,scale,scale],"spacing":side/grid as f64,
        "boundingBox":{"min":spool.min,"max":cube_max},"encoding":"DEFAULT","attributes":attrs,
        "conversion":{"validPoints":spool.points,"skippedInvalidPoints":spool.skipped,"maxDepth":MAX_DEPTH,
            "notes":"LAS extra bytes, waveform data, scanner flags and E57 images/row-column metadata are not exported. E57 colors/intensity are normalized to uint16; scan poses are applied. No coordinate reprojection is performed."}
    });
    let mut metadata_writer = BufWriter::new(File::create(output.join("metadata.json"))?);
    serde_json::to_writer_pretty(&mut metadata_writer, &metadata)?;
    metadata_writer.flush()?;
    progress(
        report,
        1.0,
        format!(
            "Converted {} points into {} octree nodes",
            spool.points,
            nodes.len()
        ),
    );
    Ok(CloudInfo {
        points: spool.points,
        bounds,
    })
}

#[cfg(test)]
#[path = "cloud/tests.rs"]
mod tests;
