//! Desktop GUI: pick videos and output, tune every setting, learn a profile,
//! analyse, and build the highlight reel with live progress.

use crate::builtin;
use crate::help;
use crate::learn;
use crate::pipeline::{self, Reporter, ShotRow, Summary};
use crate::profile::Profile;
use crate::settings::{Ballistics, MarkerStyle, ProfileSource, Settings, Units};
use anyhow::Result;
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");
const SPLASH_PNG: &[u8] = include_bytes!("../assets/splash.png");
const MAIN_SIZE: egui::Vec2 = egui::vec2(1280.0, 860.0);
const MIN_SIZE: egui::Vec2 = egui::vec2(900.0, 600.0);
const SPLASH_SIZE: egui::Vec2 = egui::vec2(720.0, 405.0);
const SPLASH_SECS: f32 = 2.2;

pub fn run() -> Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("GhillieClip — airsoft shot highlights")
        .with_app_id("ghillieclip")
        .with_inner_size(SPLASH_SIZE)
        .with_decorations(false);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions { viewport, centered: true, ..Default::default() };
    eframe::run_native("GhillieClip", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
        .map_err(|e| anyhow::anyhow!("GUI failed to start: {e}"))
}

fn png_texture(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<egui::TextureHandle> {
    let img = eframe::icon_data::from_png_bytes(png).ok()?;
    let colour = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba);
    Some(ctx.load_texture(name, colour, egui::TextureOptions::LINEAR))
}

/// Borderless start-up screen; the window then turns into the main window.
struct Splash {
    tex: Option<egui::TextureHandle>,
    start: f64,
    secs: f32,
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    let path = base.join("ghillieclip").join("settings.json");
    // Carry settings over from before the rename.
    let old = base.join("shotclip").join("settings.json");
    if !path.exists() && old.exists() {
        let _ = std::fs::create_dir_all(path.parent()?);
        let _ = std::fs::copy(&old, &path);
    }
    Some(path)
}

/// State shared with the background job thread.
struct Shared {
    ctx: egui::Context,
    log: Mutex<Vec<String>>,
    stage: Mutex<(String, usize, usize)>,
    rows: Mutex<Vec<ShotRow>>,
    cancel: AtomicBool,
    running: AtomicBool,
    result: Mutex<Option<JobResult>>,
}

enum JobResult {
    Learned(Result<Profile>),
    Ran { dry_run: bool, result: Result<Summary> },
}

impl Reporter for Shared {
    fn log(&self, msg: &str) {
        self.log.lock().unwrap().push(msg.to_string());
        self.ctx.request_repaint();
    }
    fn progress(&self, stage: &str, done: usize, total: usize) {
        *self.stage.lock().unwrap() = (stage.to_string(), done, total);
        self.ctx.request_repaint();
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn shots(&self, rows: &[ShotRow]) {
        *self.rows.lock().unwrap() = rows.to_vec();
        self.ctx.request_repaint();
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Log,
    Shots,
}

struct App {
    s: Settings,
    /// Profile currently in effect (learned, loaded or built in).
    profile: Option<Profile>,
    shared: Arc<Shared>,
    tab: Tab,
    selected_input: Option<usize>,
    status: String,
    /// Test hook: GHILLIECLIP_SCREENSHOT=<file.ppm> saves the window and quits.
    screenshot: Option<(String, u32, Arc<AtomicBool>)>,
    splash: Option<Splash>,
    /// Frames left in which to keep asking for the main window size after the
    /// splash (some window managers ignore it while decorations change).
    resize_tries: u32,
    logo: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let s = config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .unwrap_or_default();
        let shared = Arc::new(Shared {
            ctx: cc.egui_ctx.clone(),
            log: Mutex::new(vec!["Add videos, choose an output file, then Analyse or Build.".into()]),
            stage: Mutex::new((String::new(), 0, 0)),
            rows: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
            running: AtomicBool::new(false),
            result: Mutex::new(None),
        });
        let screenshot = std::env::var("GHILLIECLIP_SCREENSHOT").ok().map(|p| (p, 0, Arc::new(AtomicBool::new(false))));
        // GHILLIECLIP_SCREENSHOT_SPLASH=1 keeps the splash up so the screenshot hook captures it.
        let keep_splash = std::env::var_os("GHILLIECLIP_SCREENSHOT_SPLASH").is_some();
        let splash = Splash {
            tex: png_texture(&cc.egui_ctx, "splash", SPLASH_PNG),
            start: if keep_splash { f64::INFINITY } else { 0.0 },
            // The screenshot hook captures the main window straight away.
            secs: if screenshot.is_some() && !keep_splash { 0.0 } else { SPLASH_SECS },
        };
        let logo = png_texture(&cc.egui_ctx, "logo", ICON_PNG);
        let mut app = App {
            s,
            profile: None,
            shared,
            tab: Tab::Log,
            selected_input: None,
            status: String::new(),
            screenshot,
            splash: Some(splash),
            resize_tries: 0,
            logo,
        };
        app.reload_profile_if_static();
        app
    }

    /// Draws the splash; returns false once it is over and the main window is set up.
    fn splash_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(sp) = &mut self.splash else { return false };
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        if sp.start == 0.0 {
            sp.start = now;
        }
        let t = if sp.start.is_finite() { (now - sp.start) as f32 } else { 1.0 };
        let rect = ui.max_rect();
        let skip = ui.interact(rect, ui.scope_id().with("splash"), egui::Sense::click()).clicked();
        let secs = sp.secs;
        if t >= secs || skip {
            self.splash = None;
            self.resize_tries = 30;
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(MAIN_SIZE));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(MIN_SIZE));
            if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
                let pos = ((m - MAIN_SIZE) * 0.5).max(egui::Vec2::ZERO);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos.to_pos2()));
            }
            ctx.request_repaint();
            return false;
        }
        let fade = (t / 0.35).min(1.0).min((secs - t) / 0.3).clamp(0.0, 1.0);
        let p = ui.painter();
        p.rect_filled(rect, 0.0, egui::Color32::from_rgb(14, 26, 16));
        if let Some(tex) = &sp.tex {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            p.image(tex.id(), rect, uv, egui::Color32::WHITE.gamma_multiply(fade));
        }
        // Loading bar along the rule under the tagline (artwork is 1200×675).
        let k = rect.width() / 1200.0;
        let (x0, x1, y) = (rect.left() + 504.0 * k, rect.left() + 1144.0 * k, rect.top() + 450.0 * k);
        let progress = (t / (secs - 0.3)).clamp(0.0, 1.0);
        let amber = egui::Color32::from_rgb(255, 168, 38).gamma_multiply(fade);
        p.line_segment([egui::pos2(x0, y), egui::pos2(x0 + (x1 - x0) * progress, y)], egui::Stroke::new(3.0, amber));
        let faint = egui::Color32::from_rgb(170, 186, 150).gamma_multiply(fade);
        p.text(egui::pos2(x0, y + 14.0), egui::Align2::LEFT_TOP, "Loading…", egui::FontId::proportional(13.0), faint);
        p.text(
            egui::pos2(rect.right() - 16.0, rect.bottom() - 12.0),
            egui::Align2::RIGHT_BOTTOM,
            concat!("v", env!("CARGO_PKG_VERSION")),
            egui::FontId::proportional(13.0),
            faint,
        );
        ctx.request_repaint();
        true
    }

    fn running(&self) -> bool {
        self.shared.running.load(Ordering::Relaxed)
    }

    fn log(&self, msg: impl Into<String>) {
        self.shared.log.lock().unwrap().push(msg.into());
    }

    fn save_config(&self) {
        if let Some(p) = config_path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            if let Ok(t) = serde_json::to_string_pretty(&self.s) {
                let _ = std::fs::write(p, t);
            }
        }
    }

    /// Built-in and file profiles are available immediately.
    fn reload_profile_if_static(&mut self) {
        self.profile = match &self.s.profile {
            ProfileSource::BuiltIn => Some(builtin::profile()),
            ProfileSource::File(p) => match Profile::load(p) {
                Ok(pr) => Some(pr),
                Err(e) => {
                    self.log(format!("Could not load profile: {e:#}"));
                    None
                }
            },
            ProfileSource::Learn => None,
        };
    }

    fn use_profile(&mut self, p: Profile) {
        p.apply(&mut self.s);
        self.log(format!("Profile in use: {}", p.summary()));
        self.profile = Some(p);
    }

    fn start(&mut self, kind: &'static str) {
        if self.running() {
            return;
        }
        self.save_config();
        let sh = self.shared.clone();
        sh.cancel.store(false, Ordering::Relaxed);
        sh.running.store(true, Ordering::Relaxed);
        *sh.stage.lock().unwrap() = (String::from("Starting"), 0, 0);
        let s = self.s.clone();
        let profile = self.profile.clone();
        self.tab = if kind == "learn" { Tab::Log } else { Tab::Shots };
        if kind != "learn" {
            sh.rows.lock().unwrap().clear();
        }
        std::thread::spawn(move || {
            let result = match kind {
                "learn" => JobResult::Learned((|| {
                    pipeline::check_tools()?;
                    let files = pipeline::expand_inputs(&s.inputs, &s.output)?;
                    if files.is_empty() {
                        anyhow::bail!("no video files selected");
                    }
                    learn::learn(&files, &s, &*sh)
                })()),
                _ => {
                    let dry_run = kind == "analyse";
                    JobResult::Ran { dry_run, result: pipeline::run(&s, profile, dry_run, &*sh) }
                }
            };
            *sh.result.lock().unwrap() = Some(result);
            sh.running.store(false, Ordering::Relaxed);
            sh.ctx.request_repaint();
        });
    }

    fn collect_result(&mut self) {
        let Some(r) = self.shared.result.lock().unwrap().take() else { return };
        match r {
            JobResult::Learned(Ok(p)) => {
                self.status = "Profile learned".into();
                self.use_profile(p);
            }
            JobResult::Ran { dry_run, result: Ok(sum) } => {
                if self.profile.is_none() {
                    if let Some(p) = sum.profile.clone() {
                        self.use_profile(p);
                    }
                }
                self.status = match (&sum.output, dry_run) {
                    (Some(o), _) => format!("Done: {} shot(s), wrote {}", sum.shots.len(), o.display()),
                    _ => format!("Analysis done: {} shot(s) in {} clip(s)", sum.shots.len(), sum.segments),
                };
                self.log(self.status.clone());
            }
            JobResult::Learned(Err(e)) | JobResult::Ran { result: Err(e), .. } => {
                self.status = if e.to_string() == "cancelled" { "Cancelled".into() } else { format!("Error: {e:#}") };
                self.log(self.status.clone());
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.collect_result();
        if let Some((path, n, done)) = &mut self.screenshot {
            *n += 1;
            // Test hook: GHILLIECLIP_AUTORUN=analyse|build starts a job, and the
            // screenshot waits until it has finished.
            let autorun = std::env::var("GHILLIECLIP_AUTORUN").ok();
            if *n == 2 {
                if let Some(kind) = autorun.as_deref() {
                    let kind: &'static str = if kind == "build" { "build" } else { "analyse" };
                    let (p, d) = (path.clone(), done.clone());
                    self.start(kind);
                    self.screenshot = Some((p, 3, d));
                    return;
                }
            }
            let (path, n, done) = self.screenshot.as_mut().unwrap();
            if autorun.is_some() && self.shared.running.load(Ordering::Relaxed) {
                *n = (*n).min(9);
            }
            if *n == 40 {
                let (path, done) = (path.clone(), done.clone());
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                    move |img| {
                        let mut out = format!("P6 {} {} 255\n", img.size[0], img.size[1]).into_bytes();
                        out.extend(img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]));
                        let _ = std::fs::write(&path, out);
                        done.store(true, Ordering::Relaxed);
                    },
                )));
            }
            if done.load(Ordering::Relaxed) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ui.ctx().request_repaint();
        }
        if self.splash_ui(ui) {
            return;
        }
        if self.resize_tries > 0 {
            self.resize_tries -= 1;
            // The root Ui spans the whole window.
            let size = ui.max_rect().size();
            if (size - MAIN_SIZE).length() > 2.0 {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(MAIN_SIZE));
                ui.ctx().request_repaint();
            } else {
                self.resize_tries = 0;
            }
        }
        let running = self.running();
        if running {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }

        egui::Panel::top("actions").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(26.0, 26.0)));
                }
                ui.heading("GhillieClip");
                ui.separator();
                let ready = !running && !self.s.inputs.is_empty();
                if ui.add_enabled(ready, egui::Button::new("🎓 Learn profile")).on_hover_text(
                    "Listen to the selected footage to learn what your gun's shot sounds like and where its BB appears",
                ).clicked() {
                    self.start("learn");
                }
                if ui.add_enabled(ready, egui::Button::new("🔍 Analyse")).on_hover_text(
                    "Detect shots and track BBs without writing a video",
                ).clicked() {
                    self.start("analyse");
                }
                if ui.add_enabled(ready, egui::Button::new("🎬 Build highlight reel")).clicked() {
                    self.start("build");
                }
                if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                    self.shared.cancel.store(true, Ordering::Relaxed);
                }
            });
            let (stage, done, total) = self.shared.stage.lock().unwrap().clone();
            if running {
                let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                ui.add(egui::ProgressBar::new(frac).text(format!("{stage} {done}/{total}")).show_percentage());
            } else if !self.status.is_empty() {
                ui.label(&self.status);
            }
            ui.add_space(4.0);
        });

        egui::Panel::left("settings").resizable(true).default_size(470.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_enabled_ui(!running, |ui| self.settings_ui(ui));
            });
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| self.files_ui(ui));
            ui.separator();
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Log, "Log");
                let n = self.shared.rows.lock().unwrap().len();
                ui.selectable_value(&mut self.tab, Tab::Shots, format!("Shots ({n})"));
            });
            match self.tab {
                Tab::Log => {
                    egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false; 2]).show(ui, |ui| {
                        for l in self.shared.log.lock().unwrap().iter() {
                            ui.monospace(l);
                        }
                    });
                }
                Tab::Shots => self.shots_ui(ui),
            }
        });
    }

    fn on_exit(&mut self) {
        self.save_config();
    }
}

/// A small ℹ button; clicking it opens a popup with the setting's full explanation.
fn info(ui: &mut egui::Ui, title: &str, text: &str) {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("What does this do?");
    let accent = ui.visuals().hyperlink_color;
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&r));
    let p = ui.painter();
    if r.hovered() || open {
        p.circle_filled(rect.center(), 7.5, accent);
    } else {
        p.circle_stroke(rect.center(), 7.0, egui::Stroke::new(1.3, accent));
    }
    let glyph = if r.hovered() || open { ui.visuals().extreme_bg_color } else { accent };
    p.text(rect.center(), egui::Align2::CENTER_CENTER, "i", egui::FontId::proportional(11.5), glyph);
    egui::Popup::from_toggle_button_response(&r)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(360.0);
            ui.strong(title.trim_start_matches("… "));
            ui.add_space(2.0);
            ui.label(text);
        });
}

/// A settings grid row: label, control, ℹ.
fn row(ui: &mut egui::Ui, label: &str, help: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.label(label);
    control(ui);
    info(ui, label, help);
    ui.end_row();
}

/// A labelled numeric field with an ℹ explanation.
fn num(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, speed: f32, suffix: &str, help: &str) {
    row(ui, label, help, |ui| {
        ui.add(egui::DragValue::new(v).range(range).speed(speed).suffix(suffix));
    });
}

fn colour(ui: &mut egui::Ui, label: &str, c: &mut [u8; 3], help: &str) {
    row(ui, label, help, |ui| {
        ui.color_edit_button_srgb(c);
    });
}

fn grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, add);
}

impl App {
    fn files_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Videos");
            if ui.button("Add files…").clicked() {
                if let Some(v) = rfd::FileDialog::new()
                    .add_filter("Video", &["mp4", "MP4", "mov", "MOV", "mkv", "m4v"])
                    .pick_files()
                {
                    self.s.inputs.extend(v);
                }
            }
            if ui.button("Add folder…").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.s.inputs.push(d);
                }
            }
            if ui.add_enabled(self.selected_input.is_some(), egui::Button::new("Remove")).clicked() {
                if let Some(i) = self.selected_input.take() {
                    if i < self.s.inputs.len() {
                        self.s.inputs.remove(i);
                    }
                }
            }
            if ui.button("Clear").clicked() {
                self.s.inputs.clear();
                self.selected_input = None;
            }
        });
        egui::ScrollArea::vertical().id_salt("inputs").max_height(140.0).auto_shrink([false, true]).show(ui, |ui| {
            if self.s.inputs.is_empty() {
                ui.weak("No videos yet: add files or a folder.");
            }
            for (i, p) in self.s.inputs.iter().enumerate() {
                let label = if p.is_dir() { format!("📁 {}", p.display()) } else { format!("🎞 {}", p.display()) };
                if ui.selectable_label(self.selected_input == Some(i), label).clicked() {
                    self.selected_input = Some(i);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.strong("Output");
            let mut text = self.s.output.display().to_string();
            if ui.add(egui::TextEdit::singleline(&mut text).desired_width(ui.available_width() - 90.0)).changed() {
                self.s.output = PathBuf::from(text);
            }
            if ui.button("Save as…").clicked() {
                let mut d = rfd::FileDialog::new().add_filter("MP4 video", &["mp4"]).set_file_name("shots_highlight.mp4");
                if let Some(first) = self.s.inputs.first() {
                    let dir = if first.is_dir() { first.clone() } else { first.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
                    d = d.set_directory(dir);
                }
                if let Some(p) = d.save_file() {
                    self.s.output = p;
                }
            }
        });
    }

    fn shots_ui(&mut self, ui: &mut egui::Ui) {
        let rows = self.shared.rows.lock().unwrap().clone();
        egui::ScrollArea::both().auto_shrink([false; 2]).show(ui, |ui| {
            egui::Grid::new("shots").num_columns(6).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                for h in ["File", "Time", "Match", "Level", "BB frames", "Impact"] {
                    ui.strong(h);
                }
                ui.end_row();
                for r in &rows {
                    ui.label(r.file.file_name().unwrap_or_default().to_string_lossy());
                    let t = r.time;
                    ui.label(format!("{:02}:{:06.3}", (t / 60.0) as u32, t % 60.0));
                    ui.label(format!("{:.2}", r.score));
                    ui.label(format!("{:.2}", r.level));
                    ui.label(r.bb_frames.map_or("–".into(), |n| if n == 0 { "not found".into() } else { n.to_string() }));
                    ui.label(r.impact_m.map_or("–".into(), |d| Ballistics::format(d, &self.s.distance)));
                    ui.end_row();
                }
            });
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Gun profile").default_open(true).show(ui, |ui| {
            let s = &mut self.s;
            let before = s.profile.clone();
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.profile, ProfileSource::Learn, "Learn from the selected footage");
                info(ui, "Learn from the selected footage", help::PROFILE_LEARN);
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.profile, ProfileSource::BuiltIn, "Built-in (SC024K HPA sniper rifle)");
                info(ui, "Built-in profile", help::PROFILE_BUILTIN);
            });
            let is_file = matches!(s.profile, ProfileSource::File(_));
            ui.horizontal(|ui| {
                if ui.radio(is_file, "Saved profile").clicked() || (ui.button("Load…").clicked()) {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Profile", &["json"]).pick_file() {
                        s.profile = ProfileSource::File(p);
                    }
                }
                info(ui, "Saved profile", help::PROFILE_FILE);
                if let ProfileSource::File(p) = &s.profile {
                    ui.weak(p.file_name().unwrap_or_default().to_string_lossy());
                }
            });
            let changed = before != s.profile;
            let summary = self.profile.as_ref().map(|p| p.summary());
            let can_save = self.profile.is_some();
            match summary {
                Some(t) => ui.small(t),
                None => ui.small("Not learned yet: press “Learn profile”, or it will be learned when you Analyse/Build."),
            };
            ui.horizontal(|ui| {
                if ui.add_enabled(can_save, egui::Button::new("Save profile…")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Profile", &["json"]).set_file_name("gun_profile.json").save_file() {
                        if let Some(pr) = &self.profile {
                            match pr.save(&p) {
                                Ok(()) => self.shared.log.lock().unwrap().push(format!("Saved profile to {}", p.display())),
                                Err(e) => self.shared.log.lock().unwrap().push(format!("Could not save profile: {e:#}")),
                            }
                        }
                    }
                }
                info(ui, "Save profile", help::PROFILE_SAVE);
            });
            if changed {
                self.reload_profile_if_static();
                if let Some(p) = self.profile.clone() {
                    self.use_profile(p);
                }
            }
        });
        let s = &mut self.s;

        egui::CollapsingHeader::new("Clip timing").default_open(true).show(ui, |ui| {
            grid(ui, "clips", |ui| {
                num(ui, "Before each shot", &mut s.clips.pre_secs, 0.0..=60.0, 0.1, " s", help::PRE_SECS);
                num(ui, "After each shot", &mut s.clips.post_secs, 0.0..=60.0, 0.1, " s", help::POST_SECS);
            });
        });

        egui::CollapsingHeader::new("Shot detection").show(ui, |ui| {
            grid(ui, "detect", |ui| {
                num(ui, "Sound match ≥", &mut s.detect.threshold, 0.3..=1.0, 0.005, "", help::THRESHOLD);
                num(ui, "Loudness ≥", &mut s.detect.min_rel_level, 0.0..=2.0, 0.01, "× typical", help::MIN_REL_LEVEL);
                num(ui, "High-freq share ≤", &mut s.detect.max_hf, 0.0..=1.0, 0.005, "", help::MAX_HF);
                num(ui, "Min gap between shots", &mut s.detect.min_shot_gap, 0.02..=3.0, 0.01, " s", help::MIN_SHOT_GAP);
                num(ui, "Onset sharpness", &mut s.detect.onset_ratio, 2.0..=40.0, 0.1, "×", help::ONSET_RATIO);
                num(ui, "Require BB in flight for", &mut s.detect.require_bb_secs, 0.0..=5.0, 0.05, " s", help::REQUIRE_BB);
            });
        });

        egui::CollapsingHeader::new("BB highlight & tracking").default_open(true).show(ui, |ui| {
            grid(ui, "bb", |ui| {
                row(ui, "Highlight the BB", help::HIGHLIGHT, |ui| {
                    ui.checkbox(&mut s.track.highlight, "");
                });
                num(ui, "Highlight size", &mut s.track.highlight_size_pct, 10.0..=2000.0, 1.0, " %", help::HIGHLIGHT_SIZE);
                colour(ui, "Highlight colour", &mut s.track.highlight_colour, help::HIGHLIGHT_COLOUR);
                num(ui, "Smallest highlight radius", &mut s.track.highlight_min_radius, 0.5..=50.0, 0.1, " px", help::HIGHLIGHT_MIN_RADIUS);
                row(ui, "Follow sensitivity", help::SENSITIVITY, |ui| {
                    let mut sens = s.track.sensitivity;
                    if ui.add(egui::Slider::new(&mut sens, 0.0..=1.0)).changed() {
                        s.track = s.track.with_sensitivity(sens);
                    }
                });
                num(ui, "Follow for up to", &mut s.track.max_flight_secs, 0.2..=6.0, 0.05, " s", help::MAX_FLIGHT);
            });
            egui::CollapsingHeader::new("Advanced tracking").show(ui, |ui| {
                let t = &mut s.track;
                grid(ui, "bb_adv", |ui| {
                    ui.strong("Find phase");
                    ui.end_row();
                    num(ui, "Min contrast", &mut t.find_contrast, 1.0..=100.0, 0.1, "", help::FIND_CONTRAST);
                    num(ui, "Static ratio", &mut t.find_static_ratio, 0.05..=1.5, 0.01, "", help::FIND_STATIC_RATIO);
                    num(ui, "Surround change", &mut t.find_ring_change, 0.05..=2.0, 0.01, "", help::FIND_RING_CHANGE);
                    num(ui, "BB appears within", &mut t.find_seed_secs, 0.05..=2.0, 0.01, " s", help::FIND_SEED);
                    num(ui, "Late pick-up within", &mut t.find_late_secs, 0.0..=3.0, 0.05, " s", help::FIND_LATE);
                    num(ui, "Late pick-up radius", &mut t.find_late_radius, 0.01..=1.0, 0.005, " × width", help::FIND_LATE_RADIUS);
                    row(ui, "Late track min detections", help::FIND_LATE_MIN, |ui| {
                        ui.add(egui::DragValue::new(&mut t.find_late_min_detections).range(6..=60));
                    });
                    ui.strong("Follow phase");
                    ui.end_row();
                    num(ui, "Min contrast", &mut t.follow_contrast, 1.0..=100.0, 0.1, "", help::FOLLOW_CONTRAST);
                    num(ui, "Search radius", &mut t.follow_radius, 1.0..=200.0, 0.1, " px", help::FOLLOW_RADIUS);
                    num(ui, "Radius per speed", &mut t.follow_radius_per_speed, 0.0..=5.0, 0.01, "", help::FOLLOW_RADIUS_SPEED);
                    row(ui, "Max missing frames", help::FOLLOW_MAX_GAP, |ui| {
                        ui.add(egui::DragValue::new(&mut t.follow_max_gap).range(0..=60));
                    });
                    num(ui, "Max acceleration", &mut t.follow_max_accel, 0.1..=50.0, 0.05, " px/f²", help::FOLLOW_MAX_ACCEL);
                    num(ui, "Max brightening", &mut t.follow_max_brightening, 1.0..=10.0, 0.05, "×", help::FOLLOW_MAX_BRIGHTENING);
                    num(ui, "Min fade", &mut t.follow_min_fade, 0.0..=1.0, 0.01, "×", help::FOLLOW_MIN_FADE);
                    num(ui, "Re-acquire radius", &mut t.follow_reacquire_radius, 0.1..=2.0, 0.01, "×", help::FOLLOW_REACQ_RADIUS);
                    num(ui, "Re-acquire brightening", &mut t.follow_reacquire_brightening, 1.0..=10.0, 0.05, "×", help::FOLLOW_REACQ_BRIGHTENING);
                    num(ui, "Max speed gain", &mut t.follow_max_speed_gain, 1.0..=20.0, 0.05, "×", help::FOLLOW_MAX_SPEED_GAIN);
                    num(ui, "Novelty distance", &mut t.follow_novelty_px, 2.0..=100.0, 0.5, " px", help::FOLLOW_NOVELTY);
                    num(ui, "Static ratio", &mut t.follow_static_ratio, 0.05..=1.5, 0.01, "", help::FOLLOW_STATIC_RATIO);
                    num(ui, "Surround change", &mut t.follow_ring_change, 0.05..=2.0, 0.01, "", help::FOLLOW_RING_CHANGE);
                    ui.strong("Launch point");
                    ui.end_row();
                    num(ui, "X", &mut t.launch_x, 0.0..=1.0, 0.001, " × width", help::LAUNCH_X);
                    num(ui, "Y", &mut t.launch_y, 0.0..=1.0, 0.001, " × height", help::LAUNCH_Y);
                    num(ui, "Radius", &mut t.launch_radius, 0.005..=1.0, 0.001, " × width", help::LAUNCH_RADIUS);
                    num(ui, "Direction", &mut t.launch_dir_deg, -180.0..=180.0, 0.5, "°", help::LAUNCH_DIR);
                    num(ui, "Direction tolerance", &mut t.launch_dir_tol_deg, 1.0..=180.0, 0.5, "°", help::LAUNCH_DIR_TOL);
                });
            });
        });

        egui::CollapsingHeader::new("Impact markers").default_open(true).show(ui, |ui| {
            grid(ui, "markers", |ui| {
                row(ui, "Show impact markers", help::MARKERS, |ui| {
                    ui.checkbox(&mut s.markers.enabled, "");
                });
                row(ui, "Style", help::MARKER_STYLE, |ui| {
                    egui::ComboBox::new("marker_style", "").selected_text(s.markers.style.label()).show_ui(ui, |ui| {
                        for st in MarkerStyle::ALL {
                            ui.selectable_value(&mut s.markers.style, st, st.label());
                        }
                    });
                });
                colour(ui, "Colour", &mut s.markers.colour, help::MARKER_COLOUR);
                num(ui, "Size", &mut s.markers.size_px, 5.0..=1000.0, 0.5, " px", help::MARKER_SIZE);
                num(ui, "Duration", &mut s.markers.duration_secs, 0.05..=10.0, 0.01, " s", help::MARKER_DURATION);
                num(ui, "Sudden stop ratio", &mut s.track.impact_stop_ratio, 0.0..=1.0, 0.01, "×", help::IMPACT_STOP);
                num(ui, "… if moving faster than", &mut s.track.impact_stop_min_speed, 0.0..=50.0, 0.1, " px/frame", help::IMPACT_STOP_MIN);
                num(ui, "… and then slower than", &mut s.track.impact_stop_max_speed, 0.0..=20.0, 0.05, " px/frame", help::IMPACT_STOP_MAX);
                num(ui, "Sudden dimming", &mut s.track.impact_dim_ratio, 0.0..=1.0, 0.01, "×", help::IMPACT_DIM);
                num(ui, "Vanish ratio", &mut s.track.impact_vanish_ratio, 0.5..=50.0, 0.05, "×", help::IMPACT_VANISH);
                num(ui, "Scene change", &mut s.track.impact_change, 0.5..=100.0, 0.1, "", help::IMPACT_CHANGE);
            });
        });

        egui::CollapsingHeader::new("Distance").default_open(true).show(ui, |ui| {
            grid(ui, "distance", |ui| {
                row(ui, "Show at impact", help::DIST_AT_IMPACT, |ui| {
                    ui.checkbox(&mut s.distance.at_impact, "");
                });
                row(ui, "Show next to BB every frame", help::DIST_LIVE, |ui| {
                    ui.checkbox(&mut s.distance.live, "");
                });
                row(ui, "Units", help::DIST_UNITS, |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut s.distance.units, Units::Metres, "metres");
                        ui.radio_value(&mut s.distance.units, Units::Feet, "feet");
                    });
                });
                num(ui, "Font size", &mut s.distance.font_px, 6.0..=400.0, 0.5, " px", help::DIST_FONT);
                colour(ui, "Text colour", &mut s.distance.colour, help::DIST_COLOUR);
                row(ui, "Decimal places", help::DIST_DECIMALS, |ui| {
                    ui.add(egui::DragValue::new(&mut s.distance.decimals).range(0..=3));
                });
                ui.strong("BB ballistics");
                ui.end_row();
                num(ui, "BB mass", &mut s.ballistics.bb_mass_g, 0.05..=2.0, 0.005, " g", help::BB_MASS);
                num(ui, "Muzzle velocity", &mut s.ballistics.muzzle_fps, 50.0..=1000.0, 1.0, " fps", help::MUZZLE_FPS);
                num(ui, "BB diameter", &mut s.ballistics.bb_diameter_mm, 4.0..=8.0, 0.01, " mm", help::BB_DIAMETER);
                num(ui, "Drag coefficient", &mut s.ballistics.drag_coefficient, 0.05..=1.5, 0.005, "", help::DRAG);
                num(ui, "Air density", &mut s.ballistics.air_density, 0.5..=1.5, 0.001, " kg/m³", help::AIR_DENSITY);
            });
            let b = s.ballistics;
            let d = s.distance;
            ui.small(format!(
                "Flight model: after 0.25 s {} at {:.0} fps; after 0.5 s {} at {:.0} fps; after 1 s {}",
                Ballistics::format(b.distance_m(0.25), &d),
                b.speed_ms(0.25) / 0.3048,
                Ballistics::format(b.distance_m(0.5), &d),
                b.speed_ms(0.5) / 0.3048,
                Ballistics::format(b.distance_m(1.0), &d),
            ));
        });

        egui::CollapsingHeader::new("Encoding").show(ui, |ui| {
            grid(ui, "encode", |ui| {
                row(ui, "Fast copy (no overlays)", help::COPY, |ui| {
                    ui.checkbox(&mut s.encode.copy, "");
                });
                row(ui, "Quality (CRF)", help::CRF, |ui| {
                    ui.add(egui::DragValue::new(&mut s.encode.crf).range(0..=51));
                });
                row(ui, "Speed preset", help::PRESET, |ui| {
                    egui::ComboBox::new("preset", "").selected_text(s.encode.preset.clone()).show_ui(ui, |ui| {
                        for p in ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow"] {
                            ui.selectable_value(&mut s.encode.preset, p.to_string(), p);
                        }
                    });
                });
                row(ui, "Parallel BB tracking", help::THREADS, |ui| {
                    ui.add(egui::DragValue::new(&mut s.encode.threads).range(1..=16));
                });
            });
        });

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Save settings…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Settings", &["json"]).set_file_name("ghillieclip_settings.json").save_file() {
                    if let Ok(t) = serde_json::to_string_pretty(&self.s) {
                        let _ = std::fs::write(p, t);
                    }
                }
            }
            if ui.button("Load settings…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Settings", &["json"]).pick_file() {
                    match std::fs::read_to_string(&p).map_err(anyhow::Error::from).and_then(|t| Ok(serde_json::from_str::<Settings>(&t)?)) {
                        Ok(v) => {
                            self.s = v;
                            self.reload_profile_if_static();
                        }
                        Err(e) => self.log(format!("Could not load settings: {e:#}")),
                    }
                }
            }
            if ui.button("Reset to defaults").clicked() {
                let inputs = std::mem::take(&mut self.s.inputs);
                let output = self.s.output.clone();
                self.s = Settings { inputs, output, ..Settings::default() };
                self.profile = None;
            }
        });
    }
}
