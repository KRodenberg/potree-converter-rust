use anyhow::{bail, Context, Result};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};

use crate::model::ConversionRequest;

/// Package the browser runtime and optional models, then create a portable viewer.
pub fn write_viewer(
    project: &Path,
    request: &ConversionRequest,
    clouds: &[(String, String)],
    log: &dyn Fn(String),
) -> Result<PathBuf> {
    log("Packaging the offline viewer and third-party licenses…".into());
    let mut archive =
        zip::ZipArchive::new(Cursor::new(include_bytes!("../assets/viewer-libs.zip")))?;
    let libs = project.join("libs");
    fs::create_dir_all(&libs)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .context("Unsafe embedded viewer archive path")?;
        let target = libs.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(target)?;
        } else {
            fs::create_dir_all(target.parent().context("Missing asset parent")?)?;
            let mut output = File::create(target)?;
            std::io::copy(&mut entry, &mut output)?;
        }
    }
    let mut models = Vec::new();
    for (index, source) in request.models.iter().enumerate() {
        models.push(package_model(
            project,
            source,
            index,
            request.flip_transparency,
            log,
        )?);
    }
    let camera = request.camera.as_ref().map(|camera| {
        json!({
            "position": camera.position, "target": camera.target,
        })
    });
    let config = json!({
        "title": request.title,
        "password": request.password,
        "translation": request.translation,
        "camera": camera,
        "lights": request.lights,
        "clouds": clouds.iter().map(|(name, url)| json!({"name": name, "url": url})).collect::<Vec<_>>(),
        "models": models,
    });
    let html =
        include_str!("../assets/viewer.html").replace("{{CONFIG_JSON}}", &script_json(&config)?);
    let index = project.join("index.html");
    fs::write(&index, html)?;
    fs::write(project.join("VIEWER-README.txt"),
        "POTREE VIEWER\r\n\r\nAll browser libraries and their license notices are included in libs.\r\n\r\nPreview using the converter's Preview button, or run:\r\n  potree-converter.exe --serve \"path-to-this-project-folder\"\r\nThen open the localhost address printed by the server.\r\nA browser cannot fetch point clouds from file://; use HTTP.\r\nFor hosting, upload this entire folder without changing its structure.\r\n\r\nThe optional viewer password is a convenience screen in the HTML. It does not encrypt files or restrict direct access. Use hosting authentication for private data.\r\n\r\nOBJ copies have their own source bounds centres subtracted in double precision, then rotate +90 degrees around X in the viewer. The configured Easting/Northing/RL translation is applied after rotation. Original source models are untouched.\r\n")?;
    Ok(index)
}

fn script_json(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string(value)?
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

#[derive(Debug)]
struct ObjScan {
    min: [f64; 3],
    max: [f64; 3],
    center: [f64; 3],
    vertices: u64,
    libraries: Vec<String>,
}

fn vertex(line: &str) -> Option<([f64; 3], Vec<&str>)> {
    let mut tokens = line.split_whitespace();
    if tokens.next()? != "v" {
        return None;
    }
    let mut position = [0.0; 3];
    for coordinate in &mut position {
        *coordinate = tokens.next()?.parse::<f64>().ok()?;
        if !coordinate.is_finite() {
            return None;
        }
    }
    Some((position, tokens.collect()))
}

fn scan_obj(source: &Path) -> Result<ObjScan> {
    let mut result = ObjScan {
        min: [f64::INFINITY; 3],
        max: [f64::NEG_INFINITY; 3],
        center: [0.0; 3],
        vertices: 0,
        libraries: Vec::new(),
    };
    let mut reader = BufReader::with_capacity(1024 * 1024, File::open(source)?);
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        if let Some((position, _)) = vertex(&line) {
            for (axis, coordinate) in position.iter().enumerate() {
                result.min[axis] = result.min[axis].min(*coordinate);
                result.max[axis] = result.max[axis].max(*coordinate);
            }
            result.vertices += 1;
        } else {
            let mut fields = line.trim_start().splitn(2, char::is_whitespace);
            if fields.next() == Some("mtllib") {
                if let Some(value) = fields.next() {
                    result.libraries.push(value.trim().to_owned());
                }
            }
        }
        line.clear();
    }
    if result.vertices == 0 {
        bail!(
            "OBJ contains no finite vertex coordinates: {}",
            source.display()
        );
    }
    for axis in 0..3 {
        result.center[axis] = result.min[axis] * 0.5 + result.max[axis] * 0.5;
    }
    Ok(result)
}

fn safe_name(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            output.push(character);
        } else if !output.ends_with('_') {
            output.push('_');
        }
    }
    let output = output.trim_matches('_');
    if output.is_empty() {
        "model".into()
    } else {
        output.chars().take(100).collect()
    }
}

fn material_paths(source: &Path, scan: &ObjScan) -> Vec<PathBuf> {
    let folder = source.parent().unwrap_or(Path::new("."));
    let mut appended = source.as_os_str().to_owned();
    appended.push(".mtl");
    for candidate in [PathBuf::from(appended), source.with_extension("mtl")] {
        if candidate.is_file() {
            return vec![candidate];
        }
    }
    let mut used = HashSet::new();
    let mut materials = Vec::new();
    for reference in &scan.libraries {
        let whole = folder.join(reference.trim_matches('"'));
        let candidates = if whole.is_file() {
            vec![whole]
        } else {
            tokenize(reference)
                .unwrap_or_default()
                .iter()
                .map(|name| folder.join(name))
                .collect()
        };
        for candidate in candidates {
            if candidate.is_file() && used.insert(candidate.clone()) {
                materials.push(candidate);
            }
        }
    }
    materials
}

fn package_model(
    project: &Path,
    source: &Path,
    index: usize,
    flip: bool,
    log: &dyn Fn(String),
) -> Result<serde_json::Value> {
    let name = source
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("model");
    log(format!("Scanning OBJ bounds: {}", source.display()));
    let scan = scan_obj(source).with_context(|| format!("Reading model {}", source.display()))?;
    log(format!("{name}: {} vertices; bounds {:?} to {:?}; source centre {:?}; Potree centre {:?}. Rebased copy uses local coordinates.", scan.vertices, scan.min, scan.max, scan.center, [scan.center[0], -scan.center[2], scan.center[1]]));
    let prefix = format!("{:02}_{}", index + 1, safe_name(name));
    let directory = project.join("pointclouddata").join("models");
    fs::create_dir_all(&directory)?;
    let obj_name = format!("{prefix}.obj");
    let mtl_name = format!("{prefix}.obj.mtl");
    let materials = material_paths(source, &scan);
    if materials.is_empty() && !scan.libraries.is_empty() {
        log(format!("Warning: {name}: referenced material file was not found; default materials will be used."));
    }
    if !materials.is_empty() {
        let mut output = BufWriter::new(File::create(directory.join(&mtl_name))?);
        let mut textures = HashMap::new();
        for material in &materials {
            rewrite_material(
                material,
                &mut output,
                &directory,
                &prefix,
                flip,
                &mut textures,
                log,
            )?;
        }
        output.flush()?;
    }
    let mut output =
        BufWriter::with_capacity(1024 * 1024, File::create(directory.join(&obj_name))?);
    writeln!(
        output,
        "# Rebased by potree-converter; original source centre: {:?}",
        scan.center
    )?;
    if !materials.is_empty() {
        writeln!(output, "mtllib {mtl_name}")?;
    }
    let mut reader = BufReader::with_capacity(1024 * 1024, File::open(source)?);
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        let directive = line.split_whitespace().next();
        if directive == Some("v") {
            let (position, tail) = vertex(&line)
                .context("OBJ contains an invalid vertex; cannot safely rebase its geometry")?;
            write!(
                output,
                "v {:.17} {:.17} {:.17}",
                position[0] - scan.center[0],
                position[1] - scan.center[1],
                position[2] - scan.center[2]
            )?;
            for token in tail {
                write!(output, " {token}")?;
            }
            writeln!(output)?;
        } else if directive != Some("mtllib") {
            output.write_all(line.as_bytes())?;
            if !line.ends_with('\n') {
                writeln!(output)?;
            }
        }
        line.clear();
    }
    output.flush()?;
    Ok(
        json!({"name": name, "objUrl": format!("./pointclouddata/models/{obj_name}"), "mtlUrl": if materials.is_empty(){None}else{Some(format!("./pointclouddata/models/{mtl_name}"))}, "sourceCenter": scan.center}),
    )
}

/// MTL quoting does not use shell escapes: Windows backslashes are literal paths.
fn tokenize(value: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    for character in value.chars() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                token.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if quote.is_some() {
        return None;
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Some(tokens)
}

fn texture_reference(value: &str) -> Option<(String, Vec<String>, Vec<String>)> {
    let tokens = tokenize(value)?;
    let mut index = 0;
    let mut preserved = Vec::new();
    let mut discarded = Vec::new();
    while index < tokens.len() && tokens[index].starts_with('-') {
        let option = tokens[index].as_str();
        index += 1;
        let mut arguments = Vec::new();
        match option {
            "-s" | "-o" | "-t" => {
                while index < tokens.len()
                    && arguments.len() < 3
                    && tokens[index].parse::<f64>().is_ok()
                {
                    arguments.push(tokens[index].clone());
                    index += 1;
                }
                if arguments.is_empty() {
                    return None;
                }
                if option == "-s" || option == "-o" {
                    while arguments.len() < 3 {
                        arguments.push(if option == "-s" { "1" } else { "0" }.into());
                    }
                    preserved.push(format!("{option} {}", arguments.join(" ")));
                } else {
                    discarded.push(format!("{option} {}", arguments.join(" ")));
                }
            }
            "-mm" => {
                if index + 2 > tokens.len() {
                    return None;
                }
                arguments.extend_from_slice(&tokens[index..index + 2]);
                index += 2;
                discarded.push(format!("{option} {}", arguments.join(" ")));
            }
            "-bm" | "-blendu" | "-blendv" | "-boost" | "-texres" | "-clamp" | "-imfchan"
            | "-type" | "-cc" => {
                let argument = tokens.get(index)?;
                index += 1;
                if option == "-bm" {
                    if argument.parse::<f64>().is_err() {
                        return None;
                    }
                    preserved.push(format!("{option} {argument}"));
                } else {
                    discarded.push(format!("{option} {argument}"));
                }
            }
            _ => return None,
        }
    }
    if index == tokens.len() {
        return None;
    }
    Some((tokens[index..].join(" "), preserved, discarded))
}

fn rewrite_material(
    source: &Path,
    output: &mut impl Write,
    directory: &Path,
    prefix: &str,
    flip: bool,
    textures: &mut HashMap<PathBuf, String>,
    log: &dyn Fn(String),
) -> Result<()> {
    writeln!(
        output,
        "# Packaged material: {}",
        source.file_name().unwrap_or_default().to_string_lossy()
    )?;
    let reader = BufReader::new(File::open(source)?);
    for line in reader.lines() {
        let line = line?;
        let mut fields = line.trim().splitn(2, char::is_whitespace);
        let directive = fields.next().unwrap_or("");
        let value = fields.next().unwrap_or("").trim();
        if flip && directive.eq_ignore_ascii_case("Tr") {
            if let Some(number) = value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite())
            {
                writeln!(output, "Tr {}", 1.0 - number)?;
                continue;
            }
        }
        if matches!(
            directive.to_ascii_lowercase().as_str(),
            "map_ka"
                | "map_kd"
                | "map_ks"
                | "map_ke"
                | "map_ns"
                | "map_d"
                | "map_bump"
                | "bump"
                | "norm"
                | "disp"
                | "decal"
                | "refl"
        ) {
            if let Some((reference, options, discarded)) = texture_reference(value) {
                if reference.starts_with("http:")
                    || reference.starts_with("https:")
                    || reference.starts_with("data:")
                    || reference.starts_with("blob:")
                {
                    log(format!(
                        "Warning: external texture remains external: {reference}"
                    ));
                } else {
                    let path = source.parent().unwrap_or(Path::new(".")).join(&reference);
                    if path.is_file() {
                        let canonical = path.canonicalize()?;
                        let destination = if let Some(existing) = textures.get(&canonical) {
                            existing.clone()
                        } else {
                            let extension = path
                                .extension()
                                .and_then(|value| value.to_str())
                                .map(safe_name)
                                .unwrap_or_else(|| "bin".into());
                            let stem = path
                                .file_stem()
                                .and_then(|value| value.to_str())
                                .unwrap_or("texture");
                            let name = format!(
                                "{prefix}_texture_{}_{}.{extension}",
                                textures.len() + 1,
                                safe_name(stem)
                            );
                            fs::copy(&path, directory.join(&name))
                                .with_context(|| format!("Copying texture {}", path.display()))?;
                            textures.insert(canonical, name.clone());
                            name
                        };
                        if !discarded.is_empty() {
                            log(format!(
                                "MTL {directive}: removed loader-unsupported options {}",
                                discarded.join(" ")
                            ));
                        }
                        writeln!(
                            output,
                            "{directive} {}{destination}",
                            if options.is_empty() {
                                String::new()
                            } else {
                                format!("{} ", options.join(" "))
                            }
                        )?;
                        continue;
                    }
                    log(format!("Warning: missing texture {}", path.display()));
                }
            } else {
                log(format!(
                    "Warning: could not safely parse MTL texture directive: {directive} {value}"
                ));
            }
        }
        writeln!(output, "{line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_cannot_terminate_script_element() {
        let value = json!({"title":"</script><img src=x onerror=alert(1)>&\u{2028}"});
        let encoded = script_json(&value).unwrap();
        assert!(!encoded.contains('<'));
        assert!(!encoded.contains('&'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            value
        );
    }

    #[test]
    fn models_rebase_large_coordinates_and_package_spaced_textures() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let project = temp.path().join("output");
        fs::write(source.join("model.obj"),"mtllib model.mtl\nv 1000000 2000000 30\nv 1000000.001 2000000.002 30.003\nv 1000000.002 2000000.004 30.006\nf 1 2 3\n").unwrap();
        fs::write(source.join("diffuse color.png"), b"fixture").unwrap();
        fs::write(source.join("model.mtl"),"newmtl A\nTr 1\nmap_Kd -s 2 3 \"diffuse color.png\"\nmap_Ks diffuse color.png\nnorm -clamp on diffuse color.png\n").unwrap();
        let result = package_model(&project, &source.join("model.obj"), 0, true, &|_| {}).unwrap();
        let obj = fs::read_to_string(
            project.join(result["objUrl"].as_str().unwrap().trim_start_matches("./")),
        )
        .unwrap();
        let positions = obj.lines().filter_map(vertex).collect::<Vec<_>>();
        assert_eq!(positions.len(), 3);
        assert!((positions[0].0[0] + 0.001).abs() < 1e-9);
        assert!((positions[2].0[2] - 0.003).abs() < 1e-9);
        let mtl = fs::read_to_string(
            project.join(result["mtlUrl"].as_str().unwrap().trim_start_matches("./")),
        )
        .unwrap();
        assert!(mtl.contains("Tr 0\n"));
        assert!(mtl.contains("map_Kd -s 2 3 1 01_model_texture_1_diffuse_color.png"));
        assert!(mtl.contains("norm 01_model_texture_1_diffuse_color.png"));
        assert_eq!(
            fs::read_dir(project.join("pointclouddata/models"))
                .unwrap()
                .count(),
            3
        );
        assert!(fs::read_to_string(source.join("model.obj"))
            .unwrap()
            .contains("1000000.001"));
    }

    #[test]
    fn mtl_options_preserve_negative_numbers_and_windows_paths() {
        let (reference, preserved, discarded) =
            texture_reference("-o -2 0 -bm 0.2 -clamp on \"C:\\source images\\tex.png\"").unwrap();
        assert_eq!(reference, "C:\\source images\\tex.png");
        assert_eq!(preserved, vec!["-o -2 0 0", "-bm 0.2"]);
        assert_eq!(discarded, vec!["-clamp on"]);
        assert!(texture_reference("-unknown value texture.png").is_none());
    }
}
