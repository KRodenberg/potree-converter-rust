#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use potree_converter::{
    job,
    model::{Camera, ConversionRequest},
    server,
};
use std::{path::PathBuf, sync::atomic::AtomicBool};

#[derive(Parser)]
#[command(
    version,
    args_conflicts_with_subcommands = true,
    about = "Portable LAS / LAZ / E57 to Potree converter. Launch without arguments for the desktop GUI."
)]
struct Args {
    #[arg(long, value_name = "PROJECT_FOLDER")]
    serve: Option<PathBuf>,
    #[arg(long, default_value_t = 8080)]
    port: u16,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert point clouds without opening the GUI.
    Convert {
        #[arg(long, required = true, num_args = 1..)]
        input: Vec<PathBuf>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long, default_value = "Point Cloud Viewer")]
        title: String,
        #[arg(long, default_value = "")]
        password: String,
        #[arg(long, num_args = 1..)]
        obj: Vec<PathBuf>,
        #[arg(long, num_args = 3, allow_hyphen_values = true, default_values = ["0", "0", "0"])]
        translation: Vec<f64>,
        #[arg(
            long,
            num_args = 3,
            allow_hyphen_values = true,
            requires = "camera_target"
        )]
        camera_position: Option<Vec<f64>>,
        #[arg(
            long,
            num_args = 3,
            allow_hyphen_values = true,
            requires = "camera_position"
        )]
        camera_target: Option<Vec<f64>>,
        #[arg(long)]
        no_lights: bool,
        #[arg(long)]
        keep_transparency: bool,
        #[arg(long, default_value_t = 20_000)]
        node_points: usize,
    },
}

fn main() {
    #[cfg(windows)]
    if std::env::args_os().len() > 1 {
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        // GUI releases still support console use, inherited pipes and CLI help.
        unsafe {
            AttachConsole(u32::MAX);
        }
    }
    if let Err(error) = run() {
        eprintln!("{error:#}");
        if std::env::args_os().len() == 1 {
            rfd::MessageDialog::new()
                .set_title("Potree Converter")
                .set_description(format!("{error:#}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    if let Some(root) = args.serve {
        let preview = server::start(root, args.port)?;
        println!("Serving {} — close this process to stop.", preview.url);
        loop {
            std::thread::park();
        }
    }
    if let Some(Commands::Convert {
        input,
        output,
        job: number,
        title,
        password,
        obj,
        translation,
        camera_position,
        camera_target,
        no_lights,
        keep_transparency,
        node_points,
    }) = args.command
    {
        let request = ConversionRequest {
            inputs: input,
            models: obj,
            output_dir: output,
            job: number,
            title,
            password,
            translation: translation.try_into().unwrap(),
            camera: camera_position.zip(camera_target).map(|(p, t)| Camera {
                position: p.try_into().unwrap(),
                target: t.try_into().unwrap(),
            }),
            lights: !no_lights,
            flip_transparency: !keep_transparency,
            node_points,
        };
        job::run(&request, &AtomicBool::new(false), &|event| match event {
            job::Progress::Log(message) => println!("{message}"),
            job::Progress::Update(fraction, message) => {
                println!("{:>3.0}% {message}", fraction * 100.0)
            }
        })?;
        return Ok(());
    }
    gui::launch().map_err(|error| anyhow::anyhow!("{error}"))
}
