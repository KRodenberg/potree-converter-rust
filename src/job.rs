use crate::{cloud, model::ConversionRequest, viewer};
use anyhow::{bail, Context, Result};
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
pub enum Progress {
    Log(String),
    Update(f32, String),
}

pub fn safe_name(text: &str) -> String {
    let mut name: String = text
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    name = name.trim_matches('_').to_string();
    if name.is_empty() {
        name = "project".to_string();
    }
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if reserved.contains(&name.to_ascii_uppercase().as_str()) {
        name.insert(0, '_');
    }
    name
}

pub fn validate(request: &ConversionRequest) -> Result<()> {
    if request.inputs.is_empty() {
        bail!("Add at least one LAS, LAZ or E57 file.");
    }
    if request.job.trim().is_empty() {
        bail!("Enter a job or project number.");
    }
    if request.title.trim().is_empty() {
        bail!("Enter a viewer title.");
    }
    if !(100..=1_000_000).contains(&request.node_points) {
        bail!("Points per node must be between 100 and 1,000,000.");
    }
    for input in &request.inputs {
        if !input.is_file() {
            bail!("Input file does not exist: {}", input.display());
        }
        let ext = input
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !["las", "laz", "e57"].contains(&ext.as_str()) {
            bail!("Unsupported input: {}", input.display());
        }
    }
    for model in &request.models {
        if !model.is_file() {
            bail!("OBJ file does not exist: {}", model.display());
        }
    }
    if !request.translation.iter().all(|x| x.is_finite()) {
        bail!("Model translation must contain finite numbers.");
    }
    if let Some(camera) = &request.camera {
        if !camera
            .position
            .iter()
            .chain(camera.target.iter())
            .all(|x| x.is_finite())
        {
            bail!("Camera coordinates must contain finite numbers.");
        }
        if camera.position == camera.target {
            bail!("Camera position and target must differ.");
        }
    }
    if request.output_dir.as_os_str().is_empty() {
        bail!("Choose an output directory.");
    }
    if request.output_dir.join(safe_name(&request.job)).exists() {
        bail!("This project already exists in the output directory. Choose another job number or output directory.");
    }
    Ok(())
}

pub fn run(
    request: &ConversionRequest,
    cancel: &AtomicBool,
    report: &dyn Fn(Progress),
) -> Result<PathBuf> {
    validate(request)?;
    fs::create_dir_all(&request.output_dir).context("Cannot create output directory")?;
    let staging = tempfile::Builder::new()
        .prefix(".potree-building-")
        .tempdir_in(&request.output_dir)?;
    let project = staging.path().join("project");
    fs::create_dir_all(project.join("pointclouddata"))?;
    let job = safe_name(&request.job);
    let mut names = HashSet::new();
    let mut clouds = Vec::new();
    for (i, input) in request.inputs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("Conversion cancelled.");
        }
        let stem = input
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let base = if request.inputs.len() == 1 {
            job.clone()
        } else {
            safe_name(&format!("{job}_{stem}"))
        };
        let mut name = base.clone();
        let mut suffix = 2;
        while !names.insert(name.to_ascii_lowercase()) {
            name = format!("{base}_{suffix}");
            suffix += 1;
        }
        report(Progress::Log(format!("Reading {}", input.display())));
        let info = cloud::convert(
            input,
            &project.join("pointclouddata").join(&name),
            request.node_points,
            cancel,
            &|p| {
                report(Progress::Update(
                    (i as f32 + p.fraction) / request.inputs.len() as f32 * 0.88,
                    p.message,
                ));
            },
        )
        .with_context(|| format!("Could not convert {}", input.display()))?;
        report(Progress::Log(format!(
            "Converted {stem}: {} points; bounds {:?}",
            info.points, info.bounds
        )));
        clouds.push((stem, format!("pointclouddata/{name}/metadata.json")));
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("Conversion cancelled.");
    }
    report(Progress::Update(
        0.9,
        "Packaging viewer and model assets".into(),
    ));
    viewer::write_viewer(&project, request, &clouds, &|s| report(Progress::Log(s)))?;
    if cancel.load(Ordering::Relaxed) {
        bail!("Conversion cancelled.");
    }
    fs::write(
        project.join("conversion.txt"),
        format!(
            "Potree Converter {}\nProject: {}\nTitle: {}\nClouds: {}\nPoints per node: {}\n",
            env!("CARGO_PKG_VERSION"),
            request.job,
            request.title,
            clouds.len(),
            request.node_points
        ),
    )?;
    let destination = request.output_dir.join(&job);
    if destination.exists() {
        bail!(
            "Output appeared while converting; it has been preserved. Choose another job number."
        );
    }
    fs::rename(&project, &destination).context("Cannot publish completed project")?;
    report(Progress::Update(1.0, "Conversion complete".into()));
    report(Progress::Log(format!(
        "Project saved to {}",
        destination.display()
    )));
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_confined_and_windows_safe() {
        for input in ["..\\..//test", "CON", "NUL", " ", "a:b?c", "C:/Windows"] {
            let name = safe_name(input);
            assert!(!name.is_empty());
            assert!(!name.contains(['/', '\\', ':', '.']));
            assert!(!["CON", "NUL"].contains(&name.as_str()));
        }
    }
}
