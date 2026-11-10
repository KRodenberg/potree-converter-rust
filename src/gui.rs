use eframe::egui::{self, Color32, RichText};
use potree_converter::{
    job::{self, Progress},
    model::{Camera, ConversionRequest},
    server,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

const ACCENT: Color32 = Color32::from_rgb(84, 218, 180);
const MUTED: Color32 = Color32::from_rgb(155, 170, 186);

pub fn launch() -> eframe::Result {
    eframe::run_native(
        "Potree Converter",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([980.0, 900.0])
                .with_min_inner_size([760.0, 650.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

enum WorkerMessage {
    Progress(Progress),
    Finished(Result<PathBuf, String>),
}

struct App {
    request: ConversionRequest,
    output_text: String,
    camera_override: bool,
    camera_position: [f64; 3],
    camera_target: [f64; 3],
    log: Vec<String>,
    status: String,
    fraction: f32,
    receiver: Option<mpsc::Receiver<WorkerMessage>>,
    cancel: Arc<AtomicBool>,
    result: Option<PathBuf>,
    preview: Option<server::PreviewServer>,
    error: Option<String>,
    closing: bool,
    show_password: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = Color32::from_rgb(19, 26, 36);
        style.visuals.window_fill = Color32::from_rgb(25, 34, 46);
        style.visuals.extreme_bg_color = Color32::from_rgb(13, 20, 29);
        style.visuals.selection.bg_fill = Color32::from_rgb(36, 100, 87);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(45, 112, 96);
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        cc.egui_ctx.set_style(style);
        let output = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("PotreeOutput")))
            .unwrap_or_else(|| PathBuf::from("PotreeOutput"));
        Self {
            output_text: output.to_string_lossy().into_owned(),
            request: ConversionRequest {
                inputs: Vec::new(),
                models: Vec::new(),
                output_dir: output,
                job: String::new(),
                title: "Point Cloud Viewer".into(),
                password: String::new(),
                translation: [0.0; 3],
                camera: None,
                lights: true,
                flip_transparency: true,
                node_points: 20_000,
            },
            camera_override: false,
            camera_position: [0.0, -20.0, 15.0],
            camera_target: [0.0; 3],
            log: vec!["Ready. Add LAS, LAZ or E57 files, or drop them into this window.".into()],
            status: "Ready to convert".into(),
            fraction: 0.0,
            receiver: None,
            cancel: Arc::new(AtomicBool::new(false)),
            result: None,
            preview: None,
            error: None,
            closing: false,
            show_password: false,
        }
    }

    fn push_log(&mut self, text: String) {
        self.log.push(text);
        if self.log.len() > 2_000 {
            self.log.drain(..500);
        }
    }

    fn start(&mut self) {
        self.request.output_dir = PathBuf::from(&self.output_text);
        self.request.camera = self.camera_override.then_some(Camera {
            position: self.camera_position,
            target: self.camera_target,
        });
        if let Err(error) = job::validate(&self.request) {
            self.error = Some(format!("{error:#}"));
            return;
        }
        self.error = None;
        self.result = None;
        self.preview = None;
        self.fraction = 0.0;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let request = self.request.clone();
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.status = "Starting conversion".into();
        self.push_log(format!(
            "Starting project {} with {} input file(s).",
            request.job,
            request.inputs.len()
        ));
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                job::run(&request, &cancel, &|p| {
                    let _ = sender.send(WorkerMessage::Progress(p));
                })
            }));
            let result = match result {
                Ok(result) => result.map_err(|e| format!("{e:#}")),
                Err(_) => Err(
                    "The conversion worker stopped unexpectedly. Source files are unchanged."
                        .into(),
                ),
            };
            let _ = sender.send(WorkerMessage::Finished(result));
        });
    }

    fn poll(&mut self) {
        let messages: Vec<_> = self
            .receiver
            .as_ref()
            .map(|r| r.try_iter().collect())
            .unwrap_or_default();
        for message in messages {
            match message {
                WorkerMessage::Progress(Progress::Log(line)) => self.push_log(line),
                WorkerMessage::Progress(Progress::Update(fraction, status)) => {
                    self.fraction = fraction;
                    self.status = status;
                }
                WorkerMessage::Finished(result) => {
                    self.receiver = None;
                    match result {
                        Ok(path) => {
                            self.result = Some(path);
                            self.status = "Conversion complete".into();
                            self.fraction = 1.0;
                        }
                        Err(error) => {
                            let cancelled = self.cancel.load(Ordering::Relaxed);
                            self.status = if cancelled {
                                "Conversion cancelled"
                            } else {
                                "Conversion failed"
                            }
                            .into();
                            self.push_log(error.clone());
                            if !cancelled {
                                self.error = Some(error);
                            }
                        }
                    }
                }
            }
        }
    }

    fn open_viewer(&mut self, ctx: &egui::Context) {
        if let Some(path) = &self.result {
            match server::start(path.clone(), 0) {
                Ok(preview) => {
                    ctx.open_url(egui::OpenUrl::new_tab(&preview.url));
                    self.push_log(format!(
                        "Preview at {} (keep the converter open).",
                        preview.url
                    ));
                    self.preview = Some(preview);
                }
                Err(e) => self.error = Some(format!("Cannot start preview: {e:#}")),
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        let busy = self.receiver.is_some();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if ctx.input(|i| i.viewport().close_requested()) && busy {
            self.closing = true;
            self.cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelling before closing…".into();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.closing && !busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if !busy {
            let dropped = ctx.input(|i| i.raw.dropped_files.clone());
            for file in dropped {
                if let Some(path) = file.path {
                    let ext = path
                        .extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_ascii_lowercase();
                    let list = if ext == "obj" {
                        &mut self.request.models
                    } else if ["las", "laz", "e57"].contains(&ext.as_str()) {
                        &mut self.request.inputs
                    } else {
                        continue;
                    };
                    if !list.contains(&path) {
                        list.push(path);
                    }
                }
            }
        }
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(13, 20, 29))
                    .inner_margin(20.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("POTREE CONVERTER")
                                .size(24.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new("Point clouds to a shareable, interactive viewer")
                                .color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("LAS  /  LAZ  /  E57").color(ACCENT).strong());
                    });
                });
            });
        egui::TopBottomPanel::bottom("footer")
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(13, 20, 29))
                    .inner_margin(16.0),
            )
            .show(ctx, |ui| {
                if let Some(error) = &self.error {
                    ui.colored_label(Color32::from_rgb(255, 153, 140), error);
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&self.status).color(if self.fraction == 1.0 {
                        ACCENT
                    } else {
                        MUTED
                    }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(format!("{:.0}%", self.fraction * 100.0));
                    });
                });
                ui.add(
                    egui::ProgressBar::new(self.fraction)
                        .fill(ACCENT)
                        .desired_height(5.0),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if busy {
                        if ui.button("Cancel conversion").clicked() {
                            self.cancel.store(true, Ordering::Relaxed);
                            self.status = "Cancelling…".into();
                        }
                    } else {
                        let button = egui::Button::new(
                            RichText::new("Convert to Potree")
                                .strong()
                                .color(Color32::from_rgb(13, 35, 29)),
                        )
                        .fill(ACCENT)
                        .min_size(egui::vec2(190.0, 38.0));
                        if ui.add(button).clicked() {
                            self.start();
                        }
                    }
                    if self.result.is_some() {
                        if ui.button("Open viewer").clicked() {
                            self.open_viewer(ctx);
                        }
                        if ui.button("Open output folder").clicked() {
                            if let Some(path) = &self.result {
                                let _ =
                                    std::process::Command::new("explorer.exe").arg(path).spawn();
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("v{} · Runs locally", env!("CARGO_PKG_VERSION")))
                                .small()
                                .color(MUTED),
                        );
                    });
                });
                egui::CollapsingHeader::new("Conversion log")
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("log")
                            .max_height(110.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for line in &self.log {
                                    ui.label(
                                        RichText::new(line).monospace().size(11.0).color(MUTED),
                                    );
                                }
                            });
                    });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("settings").show(ui,|ui| {
                ui.add_enabled_ui(!busy,|ui| {
                    card(ui,"01","Point cloud files",|ui|{
                        ui.horizontal(|ui|{
                            if ui.button("+ Add files").clicked() {
                                if let Some(paths) = rfd::FileDialog::new().add_filter("Point clouds",&["las","laz","e57"]).pick_files() {
                                    for p in paths { if !self.request.inputs.contains(&p) { self.request.inputs.push(p); } }
                                }
                            }
                            if ui.add_enabled(!self.request.inputs.is_empty(),egui::Button::new("Clear list")).clicked() { self.request.inputs.clear(); }
                            ui.label(RichText::new(format!("{} file(s) · drag and drop supported",self.request.inputs.len())).small().color(MUTED));
                        });
                        file_list(ui,"cloud-files",&mut self.request.inputs);
                    });
                    card(ui,"02","Project & viewer",|ui|{
                        egui::Grid::new("project").num_columns(2).spacing([20.0,10.0]).show(ui,|ui|{
                            ui.label("Job / project number"); ui.add(egui::TextEdit::singleline(&mut self.request.job).hint_text("e.g. 260135").desired_width(f32::INFINITY)); ui.end_row();
                            ui.label("Viewer title"); ui.add(egui::TextEdit::singleline(&mut self.request.title).desired_width(f32::INFINITY));ui.end_row();
                            ui.label("Output directory");ui.horizontal(|ui|{
                                ui.add(egui::TextEdit::singleline(&mut self.output_text).desired_width((ui.available_width()-90.0).max(100.0)));
                                if ui.button("Browse…").clicked() {if let Some(p)=rfd::FileDialog::new().pick_folder(){self.output_text=p.to_string_lossy().into_owned();}}
                            });ui.end_row();
                            ui.label("Viewer password");ui.horizontal(|ui|{
                                ui.add(egui::TextEdit::singleline(&mut self.request.password).password(!self.show_password).hint_text("Optional").desired_width(250.0));
                                ui.checkbox(&mut self.show_password,"Show");
                            });ui.end_row();
                        });
                        ui.label(RichText::new("A viewer password adds a browser prompt. Use hosting authentication for private data.").small().color(MUTED));
                        ui.label(RichText::new("Each job creates a new folder. Existing projects are preserved.").small().color(MUTED));
                    });
                    card(ui,"03","OBJ models & placement",|ui|{
                        ui.horizontal(|ui|{
                            if ui.button("+ Add OBJ files").clicked() {if let Some(paths)=rfd::FileDialog::new().add_filter("OBJ models",&["obj"]).pick_files(){for p in paths {if !self.request.models.contains(&p){self.request.models.push(p);}}}}
                            ui.checkbox(&mut self.request.flip_transparency,"Flip transparency (Tr)");
                            ui.checkbox(&mut self.request.lights,"Add lights");
                        });
                        if !self.request.models.is_empty(){file_list(ui,"model-files",&mut self.request.models);}
                        ui.label(RichText::new("Post-rebase translation · keep 0, 0, 0 unless model alignment is required").small().color(MUTED));
                        xyz(ui,["Easting","Northing","RL"],&mut self.request.translation);
                    });
                    card(ui,"04","Camera & conversion settings",|ui|{
                        ui.checkbox(&mut self.camera_override,"Override automatic camera framing");
                        if self.camera_override {
                            ui.label("Camera position");xyz(ui,["X","Y","Z"],&mut self.camera_position);
                            ui.label("Look-at target (world coordinates)");xyz(ui,["X","Y","Z"],&mut self.camera_target);
                        }
                        ui.horizontal(|ui|{
                            ui.label("Target points per octree node");ui.add(egui::DragValue::new(&mut self.request.node_points).range(100..=1_000_000).speed(500));
                        });
                        ui.label(RichText::new("Conversion runs in the background. Temporary files use the output drive.").small().color(MUTED));
                    });
                });
            });
        });
    }
}

fn card(ui: &mut egui::Ui, number: &str, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(Color32::from_rgb(25, 34, 46))
        .rounding(8.0)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(number).color(ACCENT).strong());
                ui.label(RichText::new(title).strong().size(16.0));
            });
            ui.add_space(6.0);
            content(ui);
        });
    ui.add_space(10.0);
}

fn file_list(ui: &mut egui::Ui, id: &str, files: &mut Vec<PathBuf>) {
    if files.is_empty() {
        ui.label(RichText::new("No files added yet").color(MUTED).italics());
        return;
    }
    let mut remove = None;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(95.0)
        .show(ui, |ui| {
            for (i, path) in files.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(
                            path.extension()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_ascii_uppercase(),
                        )
                        .small()
                        .color(ACCENT),
                    );
                    ui.label(path.file_name().unwrap_or_default().to_string_lossy())
                        .on_hover_text(path.display().to_string());
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
        });
    if let Some(i) = remove {
        files.remove(i);
    }
}

fn xyz(ui: &mut egui::Ui, labels: [&str; 3], values: &mut [f64; 3]) {
    ui.horizontal(|ui| {
        for (label, value) in labels.into_iter().zip(values) {
            ui.label(label);
            ui.add(egui::DragValue::new(value).speed(0.25).max_decimals(6));
        }
    });
}
