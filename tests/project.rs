use potree_converter::{job, model::ConversionRequest};
use std::{fs, sync::atomic::AtomicBool};

#[test]
fn complete_projects_are_isolated_and_failed_projects_are_not_published() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    for directory in ["a", "b"] {
        let path = temp.path().join(directory);
        fs::create_dir(&path).unwrap();
        let input = path.join("same.las");
        let mut writer = las::Writer::from_path(&input, las::Header::default()).unwrap();
        writer
            .write_point(las::Point {
                x: 100.0,
                y: 200.0,
                z: 3.0,
                ..Default::default()
            })
            .unwrap();
        writer.close().unwrap();
        inputs.push(input);
    }
    let mut request = ConversionRequest {
        inputs,
        models: Vec::new(),
        output_dir: temp.path().join("output"),
        job: "job".into(),
        title: "Site </script><script>alert(1)</script>".into(),
        password: String::new(),
        translation: [0.0; 3],
        camera: None,
        lights: true,
        flip_transparency: true,
        node_points: 100,
    };
    let cancel = AtomicBool::new(false);
    let project = job::run(&request, &cancel, &|_| {}).unwrap();
    for cloud in ["job_same", "job_same_2"] {
        assert!(project
            .join("pointclouddata")
            .join(cloud)
            .join("metadata.json")
            .is_file());
    }
    assert!(project
        .join("libs/potree/workers/2.0/DecoderWorker.js")
        .is_file());
    assert!(project.join("libs/potree/LICENSE").is_file());
    let html = fs::read_to_string(project.join("index.html")).unwrap();
    assert!(!html.contains("Site </script><script>"));
    assert!(job::run(&request, &cancel, &|_| {}).is_err());
    assert!(project.join("index.html").is_file());
    let bad = temp.path().join("invalid.las");
    fs::write(&bad, b"not a LAS file").unwrap();
    request.inputs.push(bad);
    request.job = "failed-job".into();
    assert!(job::run(&request, &cancel, &|_| {}).is_err());
    assert!(!request.output_dir.join("failed-job").exists());
    assert!(fs::read_dir(&request.output_dir)
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".potree-building-")));
}
