//! Regression test pack: real clips annotated with what a person sees (shot
//! time, how long the BB is visible, impact, verified BB positions), checked
//! against what the detector and tracker produce.
//!
//! Layout:
//!   <pack>/cases/<id>.json   one annotated case
//!   <pack>/videos/<file>     the clip it refers to

use crate::audio;
use crate::bb;
use crate::builtin;
use crate::profile::Profile;
use crate::settings::Settings;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Case {
    /// Short unique name (also the file name).
    pub id: String,
    /// Clip, relative to `<pack>/videos/`.
    pub video: String,
    /// Where the clip came from.
    #[serde(default)]
    pub source: String,
    /// Free-text description and the reviewer's interpretation.
    #[serde(default)]
    pub notes: String,
    /// When the case was added (DD/MM/YYYY).
    #[serde(default)]
    pub added: String,
    /// Follow sensitivity to test at (default: the program default).
    #[serde(default)]
    pub sensitivity: Option<f32>,
    /// Profile JSON relative to the pack, or None for the built-in profile.
    #[serde(default)]
    pub profile: Option<String>,
    /// Fail if shots are detected that are not listed below.
    #[serde(default = "yes")]
    pub only_listed_shots: bool,
    pub shots: Vec<ExpectedShot>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExpectedShot {
    /// Start of the shot sound, seconds into the clip.
    pub time: f32,
    /// Last moment the reviewer can see the BB (seconds), if they can.
    #[serde(default)]
    pub bb_visible_until: Option<f32>,
    /// When the BB hits something (seconds), if it visibly does.
    #[serde(default)]
    pub impact_time: Option<f32>,
    /// Periods where the BB is hidden (e.g. crossing a hand); informational,
    /// interpolated marks there are fine.
    #[serde(default)]
    pub occlusions: Vec<[f32; 2]>,
    /// Confirmed BB positions (frame pixels).
    #[serde(default)]
    pub checkpoints: Vec<Checkpoint>,
    /// True if the reviewer says no BB is visible at all for this shot.
    #[serde(default)]
    pub bb_not_visible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub time: f32,
    pub x: f32,
    pub y: f32,
    #[serde(default = "default_tol")]
    pub tol_px: f32,
    /// Who confirmed it.
    #[serde(default)]
    pub source: String,
}

fn default_tol() -> f32 {
    10.0
}

/// Tolerances for the checks.
const SHOT_TOL: f32 = 0.05; // s, detected vs annotated shot time
const MATCH_TOL: f32 = 0.3; // s, a detection counts as this shot
const END_EARLY: f32 = 0.10; // s, track may end this much before the BB is last seen
const END_LATE: f32 = 0.10; // s, ... and this much after
const IMPACT_TOL: f32 = 0.10; // s

struct Check {
    name: String,
    pass: bool,
    detail: String,
}

pub fn load_cases(pack: &Path) -> Result<Vec<Case>> {
    let dir = pack.join("cases");
    let mut cases = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();
    for p in entries {
        let c: Case = serde_json::from_str(&std::fs::read_to_string(&p)?)
            .with_context(|| format!("parsing {}", p.display()))?;
        cases.push(c);
    }
    Ok(cases)
}

/// Run every case (or those whose id contains `filter`). Returns the report
/// and whether everything passed.
pub fn run(pack: &Path, filter: Option<&str>, sensitivity: Option<f32>) -> Result<(String, bool)> {
    let cases = load_cases(pack)?;
    let mut report = String::new();
    let (mut total, mut failed) = (0, 0);
    for case in cases.iter().filter(|c| filter.is_none_or(|f| c.id.contains(f))) {
        let checks = run_case(pack, case, sensitivity)?;
        let ok = checks.iter().all(|c| c.pass);
        let _ = writeln!(report, "{} {}  ({})", if ok { "PASS" } else { "FAIL" }, case.id, case.video);
        for c in &checks {
            let _ = writeln!(report, "    [{}] {:<34} {}", if c.pass { " ok " } else { "FAIL" }, c.name, c.detail);
        }
        total += checks.len();
        failed += checks.iter().filter(|c| !c.pass).count();
    }
    let _ = writeln!(report, "\n{} check(s), {} failed", total, failed);
    Ok((report, failed == 0))
}

fn run_case(pack: &Path, case: &Case, sensitivity: Option<f32>) -> Result<Vec<Check>> {
    let video = pack.join("videos").join(&case.video);
    if !video.exists() {
        bail!("{}: video {} missing", case.id, video.display());
    }
    let profile = match &case.profile {
        Some(p) => Profile::load(&pack.join(p))?,
        None => builtin::profile(),
    };
    let mut s = Settings::default();
    profile.apply(&mut s);
    let sens = sensitivity.or(case.sensitivity).unwrap_or(s.track.sensitivity);
    s.track = s.track.with_sensitivity(sens);
    let info = bb::probe_video(&video)?;
    let audio = audio::decode_audio(&video)?;
    let sounds = audio::detect(&audio, &profile.signature, &s.detect, |_| {});
    // Same rule as the pipeline: a sound is a shot only if its BB is
    // followed for long enough.
    let mut detected = Vec::new();
    let mut tracks = Vec::new();
    let mut rejected = Vec::new();
    let times: Vec<f32> = sounds.iter().map(|d| d.time).collect();
    for d in sounds {
        let next = times.iter().copied().filter(|&t| t > d.time).reduce(f32::min);
        let tr = bb::track_shot_before(&video, info, d.time, next, &s.track)?;
        if tracks.iter().any(|t: &bb::Track| tr.same_bb_as(t)) {
            rejected.push(format!("{:.3}s (same BB as earlier shot)", d.time));
            continue;
        }
        if s.detect.require_bb_secs <= 0.0 || tr.flight_secs(d.time, info.fps) >= s.detect.require_bb_secs {
            detected.push(d);
            tracks.push(tr);
        } else {
            rejected.push(format!("{:.3}s", d.time));
        }
    }
    let mut checks = Vec::new();
    let mut matched = vec![false; detected.len()];

    for (n, exp) in case.shots.iter().enumerate() {
        let tag = format!("shot {} @{:.3}s", n + 1, exp.time);
        let hit = detected
            .iter()
            .enumerate()
            .filter(|(_, d)| (d.time - exp.time).abs() <= MATCH_TOL)
            .min_by(|a, b| (a.1.time - exp.time).abs().total_cmp(&(b.1.time - exp.time).abs()));
        let (t, known) = match hit {
            Some((i, d)) => {
                matched[i] = true;
                let err = d.time - exp.time;
                checks.push(Check {
                    name: format!("{tag}: detected"),
                    pass: err.abs() <= SHOT_TOL,
                    detail: format!("at {:.3}s ({:+.3}s)", d.time, err),
                });
                (d.time, Some(i))
            }
            None => {
                let why = if rejected.is_empty() { String::new() } else { format!(" (sounds rejected for no BB: {})", rejected.join(", ")) };
                checks.push(Check { name: format!("{tag}: detected"), pass: false, detail: format!("not detected{why}") });
                (exp.time, None)
            }
        };

        // Track at the detected (or annotated) time.
        let track = match known {
            Some(i) => tracks[i].clone(),
            None => bb::track_shot(&video, info, t, &s.track)?,
        };
        let fps = info.fps as f32;
        let mut frames: Vec<i64> = track.marks.keys().copied().collect();
        frames.sort();
        if exp.bb_not_visible {
            checks.push(Check {
                name: format!("{tag}: no BB drawn"),
                pass: frames.is_empty(),
                detail: format!("{} frame(s) marked", frames.len()),
            });
            continue;
        }
        checks.push(Check {
            name: format!("{tag}: BB tracked"),
            pass: !frames.is_empty(),
            detail: match (frames.first(), frames.last()) {
                (Some(a), Some(b)) => format!("{:.3}s to {:.3}s ({} frames)", *a as f32 / fps, *b as f32 / fps, frames.len()),
                _ => "no track".into(),
            },
        });
        if let (Some(until), Some(&last)) = (exp.bb_visible_until, frames.last()) {
            let end = last as f32 / fps;
            let d = end - until;
            checks.push(Check {
                name: format!("{tag}: track ends when BB is lost"),
                pass: (-END_EARLY..=END_LATE).contains(&d),
                detail: format!("ends {end:.3}s vs seen until {until:.3}s ({d:+.3}s)"),
            });
        }
        if let Some(it) = exp.impact_time {
            let (pass, detail) = match &track.impact {
                Some(im) => {
                    let at = im.frame as f32 / fps;
                    ((at - it).abs() <= IMPACT_TOL, format!("at {at:.3}s vs {it:.3}s ({:+.3}s)", at - it))
                }
                None => (false, "no impact detected".into()),
            };
            checks.push(Check { name: format!("{tag}: impact"), pass, detail });
        }
        for cp in &exp.checkpoints {
            let f = (cp.time * fps).round() as i64;
            let (pass, detail) = match track.marks.get(&f) {
                Some(m) => {
                    let d = (m.x - cp.x).hypot(m.y - cp.y);
                    (d <= cp.tol_px, format!("off by {d:.1}px (≤{:.0})", cp.tol_px))
                }
                None => (false, "no mark on this frame".into()),
            };
            checks.push(Check { name: format!("{tag}: BB at {:.3}s", cp.time), pass, detail });
        }
    }
    if case.only_listed_shots {
        let extra: Vec<String> = detected
            .iter()
            .zip(&matched)
            .filter(|(_, m)| !**m)
            .map(|(d, _)| format!("{:.3}s", d.time))
            .collect();
        checks.push(Check {
            name: "no unexpected shots".into(),
            pass: extra.is_empty(),
            detail: match (extra.is_empty(), rejected.is_empty()) {
                (true, true) => "none".into(),
                (true, false) => format!("none ({} sound(s) rejected for no BB: {})", rejected.len(), rejected.join(", ")),
                _ => format!("also detected at {}", extra.join(", ")),
            },
        });
    }
    Ok(checks)
}

/// Add a case: copy (or trim) the clip into the pack and write its JSON.
#[allow(clippy::too_many_arguments)]
pub fn add_case(
    pack: &Path,
    source: &Path,
    id: &str,
    trim: Option<(f32, f32)>,
    shots: Vec<ExpectedShot>,
    notes: &str,
    added: &str,
    only_listed_shots: bool,
) -> Result<PathBuf> {
    std::fs::create_dir_all(pack.join("cases"))?;
    std::fs::create_dir_all(pack.join("videos"))?;
    let case_path = pack.join("cases").join(format!("{id}.json"));
    if case_path.exists() {
        bail!("case {} already exists", case_path.display());
    }
    let video = format!("{id}.mp4");
    let dst = pack.join("videos").join(&video);
    let offset = match trim {
        None => {
            std::fs::copy(source, &dst).with_context(|| format!("copying {}", source.display()))?;
            0.0
        }
        Some((from, to)) => {
            // Re-encode for a frame-exact cut; times become clip-relative.
            let st = crate::pipeline::tool("ffmpeg")
                .args(["-v", "error", "-nostdin", "-y", "-ss", &format!("{from:.3}"), "-i"])
                .arg(source)
                .args(["-t", &format!("{:.3}", to - from), "-map", "0:v:0", "-map", "0:a:0?"])
                .args(["-c:v", "libx264", "-preset", "medium", "-crf", "12", "-c:a", "copy"])
                .arg(&dst)
                .status()?;
            if !st.success() {
                bail!("ffmpeg failed to trim {}", source.display());
            }
            from
        }
    };
    let shots = shots
        .into_iter()
        .map(|mut s| {
            s.time -= offset;
            s.bb_visible_until = s.bb_visible_until.map(|t| t - offset);
            s.impact_time = s.impact_time.map(|t| t - offset);
            s.occlusions.iter_mut().for_each(|o| *o = [o[0] - offset, o[1] - offset]);
            s.checkpoints.iter_mut().for_each(|c| c.time -= offset);
            s
        })
        .collect();
    let case = Case {
        id: id.to_string(),
        video,
        source: match trim {
            Some((a, b)) => format!("{} ({a:.3}-{b:.3}s)", source.display()),
            None => source.display().to_string(),
        },
        notes: notes.to_string(),
        added: added.to_string(),
        sensitivity: None,
        profile: None,
        only_listed_shots,
        shots,
    };
    std::fs::write(&case_path, serde_json::to_string_pretty(&case)?)?;
    Ok(case_path)
}
