use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Camera {
    pub position: [f64; 3],
    pub target: [f64; 3],
}

#[derive(Clone, Debug)]
pub struct ConversionRequest {
    pub inputs: Vec<PathBuf>,
    pub models: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub job: String,
    pub title: String,
    pub password: String,
    pub translation: [f64; 3],
    pub camera: Option<Camera>,
    pub lights: bool,
    pub flip_transparency: bool,
    pub node_points: usize,
}
