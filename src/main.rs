//! GhillieClip — find every shot of your airsoft gun in a set of videos, track
//! the BB in flight, mark impacts, and stitch the moments into one highlight
//! reel. What a shot sounds like and where the BB appears are learned from
//! the footage itself (or taken from a saved/built-in profile).
//!
//! Run with no arguments for the GUI. Requires `ffmpeg` and `ffprobe` on PATH.

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

mod audio;
mod bb;
mod builtin;
mod gui;
mod help;
mod learn;
mod overlay;
mod pipeline;
mod profile;
mod settings;
mod testpack;

use pipeline::Reporter;
use settings::{MarkerStyle, ProfileSource, Settings, Units};

#[derive(Parser, Debug)]
#[command(
    name = "ghillieclip",
    version,
    about = "Detect airsoft shots in videos, track the BB, and build a highlight reel",
    long_about = "Detect airsoft shots in videos, track the BB, and build a highlight reel.\n\
                  Run with no arguments to open the GUI."
)]
struct Args {
    /// Video files and/or folders to process
    inputs: Vec<PathBuf>,

    /// Open the GUI (also the default when run with no arguments)
    #[arg(long)]
    gui: bool,

    /// Output highlight video
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Load every setting from a JSON file (flags below override it)
    #[arg(long, value_name = "FILE")]
    settings: Option<PathBuf>,

    /// Save the effective settings to a JSON file
    #[arg(long, value_name = "FILE")]
    save_settings: Option<PathBuf>,

    /// Use a saved profile instead of learning one
    #[arg(long, value_name = "FILE", conflicts_with = "builtin_profile")]
    profile: Option<PathBuf>,

    /// Use the built-in profile (SC024K HPA sniper rifle)
    #[arg(long)]
    builtin_profile: bool,

    /// Save the profile used (e.g. a freshly learned one) to a JSON file
    #[arg(long, value_name = "FILE")]
    save_profile: Option<PathBuf>,

    /// Only learn (or load) the profile, then exit
    #[arg(long)]
    learn_only: bool,

    /// Seconds of footage before each shot
    #[arg(long)]
    pre: Option<f32>,

    /// Seconds of footage after each shot
    #[arg(long)]
    post: Option<f32>,

    /// Minimum similarity (0..1) to the shot sound
    #[arg(long)]
    threshold: Option<f32>,

    /// Minimum loudness relative to a typical shot
    #[arg(long)]
    min_level: Option<f32>,

    /// Maximum share of energy above 4 kHz (rejects bolt clicks)
    #[arg(long)]
    max_hf: Option<f32>,

    /// A shot sound only counts if a BB is followed for this many seconds (0 = off)
    #[arg(long, value_name = "SECS")]
    require_bb: Option<f32>,

    /// BB follow sensitivity 0..1 (higher follows fainter trails, more false positives)
    #[arg(long)]
    sensitivity: Option<f32>,

    /// Do not highlight the BB
    #[arg(long)]
    no_bb: bool,

    /// Highlight diameter as % of the BB's measured diameter (200 = double)
    #[arg(long, value_name = "PERCENT")]
    bb_size: Option<f32>,

    /// Highlight colour (#RRGGBB or R,G,B)
    #[arg(long, value_parser = parse_colour)]
    bb_colour: Option<[u8; 3]>,

    /// Draw impact markers: blood, crosshair or explosion
    #[arg(long, value_name = "STYLE")]
    markers: Option<MarkerStyle>,

    /// Impact marker colour (#RRGGBB or R,G,B)
    #[arg(long, value_parser = parse_colour)]
    marker_colour: Option<[u8; 3]>,

    /// Impact marker size, px
    #[arg(long)]
    marker_size: Option<f32>,

    /// Seconds an impact marker stays on screen
    #[arg(long)]
    marker_secs: Option<f32>,

    /// Show the distance above each impact
    #[arg(long)]
    distance_at_impact: bool,

    /// Show the BB's distance next to it on every frame
    #[arg(long)]
    distance_live: bool,

    /// Distance units: m or ft
    #[arg(long)]
    units: Option<Units>,

    /// Distance text height, px
    #[arg(long)]
    font_px: Option<f32>,

    /// Distance text colour (#RRGGBB or R,G,B)
    #[arg(long, value_parser = parse_colour)]
    text_colour: Option<[u8; 3]>,

    /// BB mass, grams
    #[arg(long)]
    bb_mass: Option<f32>,

    /// Muzzle velocity, feet per second
    #[arg(long)]
    muzzle_fps: Option<f32>,

    /// Stream-copy cuts (fast; no highlights or markers)
    #[arg(long)]
    copy: bool,

    /// x264 CRF used when re-encoding
    #[arg(long)]
    crf: Option<u8>,

    /// Shots tracked in parallel
    #[arg(long)]
    threads: Option<usize>,

    /// Only report detections (and tracking); do not write a video
    #[arg(long)]
    dry_run: bool,

    /// Print the BB track for one shot and exit, e.g. --debug-track FILE.MP4@38.03
    #[arg(long, value_name = "FILE@SECONDS")]
    debug_track: Option<String>,

    /// Run the regression test pack in DIR and exit (non-zero if any check fails)
    #[arg(long, value_name = "DIR")]
    test_pack: Option<PathBuf>,

    /// Only run test cases whose id contains this text
    #[arg(long, value_name = "TEXT")]
    test_filter: Option<String>,

    /// Add a clip to the test pack (given with --pack) and exit
    #[arg(long, value_name = "VIDEO", requires_all = ["pack", "id", "shot"])]
    add_test: Option<PathBuf>,

    /// Test pack directory for --add-test
    #[arg(long, value_name = "DIR")]
    pack: Option<PathBuf>,

    /// Test case id for --add-test
    #[arg(long)]
    id: Option<String>,

    /// --add-test: start of the shot sound, seconds into the source video
    #[arg(long)]
    shot: Option<f32>,

    /// --add-test: last moment the BB is visible, seconds
    #[arg(long)]
    visible_until: Option<f32>,

    /// --add-test: when the BB hits something, seconds
    #[arg(long)]
    impact: Option<f32>,

    /// --add-test: period the BB is hidden, e.g. 2.867-2.967 (repeatable)
    #[arg(long, value_name = "FROM-TO", value_parser = parse_range)]
    occlusion: Vec<(f32, f32)>,

    /// --add-test: only keep this part of the source video, e.g. 30-40
    #[arg(long, value_name = "FROM-TO", value_parser = parse_range)]
    trim: Option<(f32, f32)>,

    /// --add-test: description / interpretation
    #[arg(long, default_value = "")]
    note: String,

    /// --add-test: other shots in the clip are allowed (not checked)
    #[arg(long)]
    allow_other_shots: bool,
}

fn parse_range(s: &str) -> Result<(f32, f32), String> {
    let (a, b) = s.split_once('-').ok_or_else(|| format!("`{s}`: expected FROM-TO"))?;
    let (a, b) = (a.trim().parse::<f32>().map_err(|e| e.to_string())?, b.trim().parse::<f32>().map_err(|e| e.to_string())?);
    if b <= a {
        return Err(format!("`{s}`: TO must be after FROM"));
    }
    Ok((a, b))
}

pub fn parse_colour(s: &str) -> Result<[u8; 3], String> {
    let t = s.trim().trim_start_matches('#');
    if t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        let v = u32::from_str_radix(t, 16).map_err(|e| e.to_string())?;
        return Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
    }
    let parts: Vec<&str> = t.split(',').map(str::trim).collect();
    if parts.len() == 3 {
        let p = |i: usize| parts[i].parse::<u8>().map_err(|e| format!("{s}: {e}"));
        return Ok([p(0)?, p(1)?, p(2)?]);
    }
    Err(format!("`{s}` is not a colour (use #RRGGBB or R,G,B)"))
}

/// Prints progress and log lines to stderr.
struct CliReporter {
    last_stage: Mutex<(String, usize)>,
    cancel: AtomicBool,
}

impl Reporter for CliReporter {
    fn log(&self, msg: &str) {
        eprintln!("{msg}");
    }
    fn progress(&self, stage: &str, done: usize, total: usize) {
        let mut last = self.last_stage.lock().unwrap();
        let pct = if total == 0 { 100 } else { done * 100 / total };
        if last.0 != stage || pct >= last.1 + 10 || done == total {
            eprintln!("[{stage}] {done}/{total}");
            *last = (stage.to_string(), pct);
        }
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Apps started from the macOS Finder or a Linux desktop launcher get a
/// minimal PATH; add the usual FFmpeg install locations.
fn extend_path() {
    #[cfg(unix)]
    {
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let before = dirs.len();
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            let d = PathBuf::from(d);
            if d.is_dir() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        if dirs.len() != before {
            if let Ok(p) = std::env::join_paths(dirs) {
                // SAFETY: called first thing in main, before any other thread exists.
                unsafe { std::env::set_var("PATH", p) };
            }
        }
    }
}

/// When the GUI is started from Explorer, Windows gives the process a console
/// window of its own; close it. A console shared with a terminal is left alone.
fn hide_own_console() {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
            fn FreeConsole() -> i32;
        }
        let mut pids = [0u32; 2];
        // SAFETY: plain Win32 calls with a valid buffer and its length.
        unsafe {
            if GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) == 1 {
                FreeConsole();
            }
        }
    }
}

fn main() -> Result<()> {
    extend_path();
    if std::env::args_os().len() == 1 {
        hide_own_console();
        return gui::run();
    }
    let args = Args::parse();
    if args.gui {
        return gui::run();
    }

    let mut s = match &args.settings {
        Some(p) => serde_json::from_str::<Settings>(&std::fs::read_to_string(p)?)
            .with_context(|| format!("parsing settings {}", p.display()))?,
        None => Settings::default(),
    };
    if let Some(pack) = &args.test_pack {
        pipeline::check_tools()?;
        let (report, ok) = testpack::run(pack, args.test_filter.as_deref(), args.sensitivity)?;
        print!("{report}");
        std::process::exit(if ok { 0 } else { 1 });
    }
    if let Some(src) = &args.add_test {
        let shot = testpack::ExpectedShot {
            time: args.shot.unwrap(),
            bb_visible_until: args.visible_until,
            impact_time: args.impact,
            occlusions: args.occlusion.iter().map(|&(a, b)| [a, b]).collect(),
            checkpoints: Vec::new(),
            bb_not_visible: false,
        };
        let path = testpack::add_case(
            args.pack.as_ref().unwrap(),
            src,
            args.id.as_ref().unwrap(),
            args.trim,
            vec![shot],
            &args.note,
            &today(),
            !args.allow_other_shots,
        )?;
        eprintln!("Added {}", path.display());
        return Ok(());
    }
    if let Some(spec) = &args.debug_track {
        apply_flags(&args, &mut s);
        return debug_track(spec, &s);
    }
    if !args.inputs.is_empty() {
        s.inputs = args.inputs.clone();
    }
    if s.inputs.is_empty() {
        bail!("no input files or folders given (run with no arguments for the GUI)");
    }
    if let Some(o) = &args.output {
        s.output = o.clone();
    }
    if let Some(p) = &args.profile {
        s.profile = ProfileSource::File(p.clone());
    } else if args.builtin_profile {
        s.profile = ProfileSource::BuiltIn;
    }

    let rep = CliReporter { last_stage: Mutex::new((String::new(), 0)), cancel: AtomicBool::new(false) };
    pipeline::check_tools()?;
    let files = pipeline::expand_inputs(&s.inputs, &s.output)?;
    if files.is_empty() {
        bail!("no video files found");
    }
    let profile = pipeline::resolve_profile(&s, &files, &rep)?;
    profile.apply(&mut s);
    apply_flags(&args, &mut s); // explicit flags win over the profile
    if let Some(p) = &args.save_profile {
        profile.save(p)?;
        eprintln!("Saved profile to {}", p.display());
    }
    if let Some(p) = &args.save_settings {
        std::fs::write(p, serde_json::to_string_pretty(&s)?)?;
        eprintln!("Saved settings to {}", p.display());
    }
    if args.learn_only {
        eprintln!("{}", profile.summary());
        return Ok(());
    }
    if s.encode.copy && (s.track.highlight || s.markers.enabled || s.distance.at_impact || s.distance.live) {
        eprintln!("Note: --copy cannot draw on frames; highlights, markers and distances are skipped");
    }

    let summary = pipeline::run(&s, Some(profile), args.dry_run, &rep)?;
    for r in &summary.shots {
        let name = r.file.file_name().unwrap_or_default().to_string_lossy();
        let bb = r.bb_frames.map_or("-".into(), |n| n.to_string());
        let imp = r.impact_m.map_or("-".into(), |d| settings::Ballistics::format(d, &s.distance));
        println!("{name}\t{:>8.3}s\tscore={:.3}\tlevel={:.2}\tbb_frames={bb}\timpact={imp}", r.time, r.score, r.level);
    }
    Ok(())
}

/// Today's date as DD/MM/YYYY (UTC).
fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{d:02}/{m:02}/{y}")
}

fn apply_flags(a: &Args, s: &mut Settings) {
    let set = |dst: &mut f32, v: Option<f32>| {
        if let Some(v) = v {
            *dst = v;
        }
    };
    set(&mut s.clips.pre_secs, a.pre);
    set(&mut s.clips.post_secs, a.post);
    set(&mut s.detect.threshold, a.threshold);
    set(&mut s.detect.min_rel_level, a.min_level);
    set(&mut s.detect.max_hf, a.max_hf);
    set(&mut s.detect.require_bb_secs, a.require_bb);
    if let Some(v) = a.sensitivity {
        s.track = s.track.with_sensitivity(v);
    }
    if a.no_bb {
        s.track.highlight = false;
    }
    set(&mut s.track.highlight_size_pct, a.bb_size);
    if let Some(c) = a.bb_colour {
        s.track.highlight_colour = c;
    }
    if let Some(m) = a.markers {
        s.markers.enabled = true;
        s.markers.style = m;
    }
    if let Some(c) = a.marker_colour {
        s.markers.colour = c;
    }
    set(&mut s.markers.size_px, a.marker_size);
    set(&mut s.markers.duration_secs, a.marker_secs);
    s.distance.at_impact |= a.distance_at_impact;
    s.distance.live |= a.distance_live;
    if let Some(u) = a.units {
        s.distance.units = u;
    }
    set(&mut s.distance.font_px, a.font_px);
    if let Some(c) = a.text_colour {
        s.distance.colour = c;
    }
    set(&mut s.ballistics.bb_mass_g, a.bb_mass);
    set(&mut s.ballistics.muzzle_fps, a.muzzle_fps);
    s.encode.copy |= a.copy;
    if let Some(c) = a.crf {
        s.encode.crf = c;
    }
    if let Some(t) = a.threads {
        s.encode.threads = t;
    }
}

fn debug_track(spec: &str, s: &Settings) -> Result<()> {
    let (file, time) = spec.rsplit_once('@').context("expected FILE@SECONDS")?;
    let time: f32 = time.parse().context("bad seconds in --debug-track")?;
    let path = Path::new(file);
    let info = bb::probe_video(path)?;
    let track = bb::track_shot(path, info, time, &s.track)?;
    eprintln!(
        "found {} + followed {} detection(s), launch {:?}, impact {}",
        track.found,
        track.followed,
        track.launch,
        track.impact.as_ref().map_or("none".into(), |i| format!(
            "frame {} at ({:.0}, {:.0}), strength {:.2}",
            i.frame, i.x, i.y, i.strength
        ))
    );
    let mut frames: Vec<_> = track.marks.iter().collect();
    frames.sort_by_key(|(f, _)| **f);
    for (f, m) in frames {
        println!("frame {f} t={:.3}s x={:.1} y={:.1} r={:.1}", *f as f64 / info.fps, m.x, m.y, m.r);
    }
    Ok(())
}
