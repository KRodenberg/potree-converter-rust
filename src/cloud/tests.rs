use super::*;
use std::collections::HashSet;

fn las_fixture(path: &Path, count: usize, duplicate: bool) -> Result<()> {
    let mut builder = las::Builder::from((1, 4));
    builder.point_format = las::point::Format::new(3)?;
    builder.transforms.x.offset = 500_000.0;
    builder.transforms.y.offset = 6_200_000.0;
    builder.transforms.z.offset = 100.0;
    let mut writer = las::Writer::from_path(path, builder.into_header()?)?;
    for i in 0..count {
        let j = if duplicate { 0 } else { i };
        writer.write_point(las::Point {
            x: 500_000.0 + (j % 10) as f64 * 0.125,
            y: 6_200_000.0 + ((j / 10) % 10) as f64 * 0.25,
            z: 100.0 + (j / 100) as f64 * 0.5,
            color: Some(las::Color::new(65535, 32768, 257)),
            intensity: 12345,
            classification: las::point::Classification::new(2)?,
            return_number: 1,
            number_of_returns: 2,
            point_source_id: 42,
            gps_time: Some(1000.0 + i as f64),
            ..las::Point::default()
        })?;
    }
    writer.close()?;
    Ok(())
}

fn verify(output: &Path, expected: usize) -> Result<(serde_json::Value, Vec<u8>)> {
    let metadata: serde_json::Value =
        serde_json::from_reader(File::open(output.join("metadata.json"))?)?;
    assert_eq!(metadata["points"].as_u64(), Some(expected as u64));
    let hierarchy = fs::read(output.join("hierarchy.bin"))?;
    let octree = fs::read(output.join("octree.bin"))?;
    assert_eq!(hierarchy.len() % 22, 0);
    assert_eq!(
        metadata["hierarchy"]["firstChunkSize"].as_u64(),
        Some(hierarchy.len() as u64)
    );
    assert_eq!(octree.len(), expected * OUTPUT_BYTES);
    let mut total = 0usize;
    let mut children = 0usize;
    let mut end = 0u64;
    let root_min: [f64; 3] =
        std::array::from_fn(|i| metadata["boundingBox"]["min"][i].as_f64().unwrap());
    let root_max: [f64; 3] =
        std::array::from_fn(|i| metadata["boundingBox"]["max"][i].as_f64().unwrap());
    let scale = metadata["scale"][0].as_f64().unwrap();
    let mut boxes = VecDeque::from([(root_min, root_max)]);
    for record in hierarchy.as_chunks::<22>().0 {
        let count = u32::from_le_bytes(record[2..6].try_into().unwrap()) as usize;
        let offset = u64::from_le_bytes(record[6..14].try_into().unwrap());
        let size = u64::from_le_bytes(record[14..22].try_into().unwrap());
        assert_eq!(offset, end);
        assert_eq!(size as usize, count * OUTPUT_BYTES);
        assert_eq!(record[0], if record[1] == 0 { 1 } else { 0 });
        assert!(count > 0);
        let (node_min, node_max) = boxes
            .pop_front()
            .expect("Hierarchy contains unreachable node");
        for child in 0..8 {
            if record[1] & (1 << child) == 0 {
                continue;
            }
            let mut min = node_min;
            let mut max = node_max;
            for axis in 0..3 {
                let middle = (min[axis] + max[axis]) / 2.0;
                if child & (4 >> axis) == 0 {
                    max[axis] = middle;
                } else {
                    min[axis] = middle;
                }
            }
            boxes.push_back((min, max));
        }
        for b in octree[offset as usize..(offset + size) as usize]
            .as_chunks::<OUTPUT_BYTES>()
            .0
        {
            for axis in 0..3 {
                let coordinate = root_min[axis]
                    + i32::from_le_bytes(b[axis * 4..axis * 4 + 4].try_into().unwrap()) as f64
                        * scale;
                assert!(
                    coordinate >= node_min[axis] - scale && coordinate <= node_max[axis] + scale,
                    "Point lies outside the octant advertised to Potree"
                );
            }
        }
        children += record[1].count_ones() as usize;
        total += count;
        end += size;
    }
    assert_eq!(total, expected);
    assert_eq!(children + 1, hierarchy.len() / 22);
    assert!(boxes.is_empty());
    assert_eq!(end as usize, octree.len());
    assert!(fs::read_dir(output)?.all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".octree-work")));
    Ok((metadata, octree))
}

#[test]
fn las_and_laz_preserve_points_and_attributes_with_real_lod() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for extension in ["las", "laz"] {
        let input = dir.path().join(format!("input.{extension}"));
        let output = dir.path().join(extension);
        las_fixture(&input, 1000, false)?;
        let info = convert(&input, &output, 64, &AtomicBool::new(false), &|_| {})?;
        assert_eq!(info.points, 1000);
        assert_eq!(info.bounds[0], [500000., 6200000., 100.]);
        let (metadata, bytes) = verify(&output, 1000)?;
        assert!(fs::metadata(output.join("hierarchy.bin"))?.len() > 22);
        let scale = metadata["scale"][0].as_f64().unwrap();
        let mut gps_values = HashSet::new();
        for b in bytes.as_chunks::<OUTPUT_BYTES>().0 {
            let gps = f64::from_le_bytes(b[25..33].try_into().unwrap());
            let index = (gps - 1000.0) as usize;
            assert!(
                gps_values.insert(index),
                "Point duplicated across octree levels"
            );
            let expected = [
                (index % 10) as f64 * 0.125,
                ((index / 10) % 10) as f64 * 0.25,
                (index / 100) as f64 * 0.5,
            ];
            for axis in 0..3 {
                let actual = i32::from_le_bytes(b[axis * 4..axis * 4 + 4].try_into().unwrap())
                    as f64
                    * scale;
                assert!(
                    (actual - expected[axis]).abs() <= scale,
                    "Coordinate precision lost"
                );
            }
            assert_eq!(u16::from_le_bytes(b[12..14].try_into().unwrap()), 65535);
            assert_eq!(u16::from_le_bytes(b[14..16].try_into().unwrap()), 32768);
            assert_eq!(u16::from_le_bytes(b[16..18].try_into().unwrap()), 257);
            assert_eq!(u16::from_le_bytes(b[18..20].try_into().unwrap()), 12345);
            assert_eq!(&b[20..23], &[2, 1, 2]);
            assert_eq!(u16::from_le_bytes(b[23..25].try_into().unwrap()), 42);
        }
        assert_eq!(gps_values.len(), 1000);
    }
    Ok(())
}

#[test]
fn duplicate_points_terminate_without_discarding_points() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("duplicates.las");
    let output = dir.path().join("cloud");
    las_fixture(&input, 33, true)?;
    convert(&input, &output, 8, &AtomicBool::new(false), &|_| {})?;
    let (meta, _) = verify(&output, 33)?;
    assert_eq!(meta["hierarchy"]["depth"].as_u64(), Some(MAX_DEPTH as u64));
    for axis in 0..3 {
        assert!(
            meta["boundingBox"]["max"][axis].as_f64().unwrap()
                > meta["boundingBox"]["min"][axis].as_f64().unwrap()
        );
    }
    Ok(())
}

#[test]
fn e57_spherical_scans_apply_pose_and_normalize_attributes() -> Result<()> {
    use e57::{Record, RecordDataType, RecordName, RecordValue};
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("scans.e57");
    let output = dir.path().join("cloud");
    let mut writer = e57::E57Writer::from_file(&input, "fixture-file")?;
    writer.set_coordinate_metadata(Some("LOCAL_CS[\"fixture\"]".to_owned()));
    for scan_index in 0..2 {
        let mut prototype = [
            RecordName::SphericalRange,
            RecordName::SphericalAzimuth,
            RecordName::SphericalElevation,
        ]
        .into_iter()
        .map(|name| Record {
            name,
            data_type: RecordDataType::Double {
                min: None,
                max: None,
            },
        })
        .collect::<Vec<_>>();
        prototype.push(Record {
            name: RecordName::SphericalInvalidState,
            data_type: RecordDataType::Integer { min: 0, max: 2 },
        });
        for name in [
            RecordName::ColorRed,
            RecordName::ColorGreen,
            RecordName::ColorBlue,
            RecordName::Intensity,
        ] {
            prototype.push(Record {
                name,
                data_type: RecordDataType::Integer { min: 0, max: 255 },
            });
        }
        let mut scan = writer.add_pointcloud(&format!("scan-{scan_index}"), prototype)?;
        scan.set_transform(Some(e57::Transform {
            translation: e57::Translation {
                x: 100.0 + scan_index as f64 * 10.0,
                y: 200.0,
                z: 300.0,
            },
            ..Default::default()
        }));
        for invalid in [0, 2] {
            scan.add_point(vec![
                RecordValue::Double(2.0),
                RecordValue::Double(0.0),
                RecordValue::Double(0.0),
                RecordValue::Integer(invalid),
                RecordValue::Integer(255),
                RecordValue::Integer(128),
                RecordValue::Integer(0),
                RecordValue::Integer(128),
            ])?;
        }
        scan.finalize()?;
    }
    writer.finalize()?;
    drop(writer);
    let info = convert(&input, &output, 64, &AtomicBool::new(false), &|_| {})?;
    assert_eq!(info.points, 2);
    assert_eq!(info.bounds, [[102.0, 200.0, 300.0], [112.0, 200.0, 300.0]]);
    let (meta, bytes) = verify(&output, 2)?;
    assert_eq!(meta["conversion"]["skippedInvalidPoints"].as_u64(), Some(2));
    assert_eq!(meta["projection"].as_str(), Some("LOCAL_CS[\"fixture\"]"));
    let sources = bytes
        .as_chunks::<OUTPUT_BYTES>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(b[23..25].try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(sources, vec![0, 1]);
    for b in bytes.as_chunks::<OUTPUT_BYTES>().0 {
        assert_eq!(u16::from_le_bytes(b[12..14].try_into().unwrap()), 65535);
        assert_eq!(u16::from_le_bytes(b[14..16].try_into().unwrap()), 32896);
        assert_eq!(u16::from_le_bytes(b[18..20].try_into().unwrap()), 32896);
    }
    Ok(())
}

#[test]
fn cancellation_and_invalid_inputs_return_errors() -> Result<()> {
    let dir = tempfile::tempdir()?;
    assert!(convert(
        &dir.path().join("missing.las"),
        &dir.path().join("a"),
        64,
        &AtomicBool::new(true),
        &|_| {}
    )
    .unwrap_err()
    .to_string()
    .contains("cancelled"));
    let input = dir.path().join("empty.las");
    las_fixture(&input, 0, false)?;
    assert!(convert(
        &input,
        &dir.path().join("b"),
        64,
        &AtomicBool::new(false),
        &|_| {}
    )
    .unwrap_err()
    .to_string()
    .contains("no valid"));
    assert!(convert(
        &input,
        &dir.path().join("c"),
        1,
        &AtomicBool::new(false),
        &|_| {}
    )
    .is_err());
    Ok(())
}
