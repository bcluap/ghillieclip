//! BB tracking and highlighting.
//!
//! After each shot the BB shows up in the scope camera as a small bright dot
//! that starts large and blurred near the muzzle, shrinks as it flies away,
//! and arcs down as it slows. For every shot we:
//!   1. decode greyscale frames from just before the shot,
//!   2. estimate the recoil shake of each frame against a pre-shot reference,
//!   3. FIND phase: over the first fraction of a second, find small bright
//!      spots that are new (absent from the shake-compensated reference and
//!      from a few frames earlier) and link them into the strongest smooth
//!      trajectory that leaves the launch point in the expected direction,
//!   4. FOLLOW phase: extend that trajectory frame by frame with a guided
//!      search around a curved (quadratic) prediction, accepting fainter spots
//!      according to `sensitivity`,
//! and later paint a bright white disc, scaled from the BB's size, on those frames.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;

const PRE_FRAMES: i64 = 6; // reference frame is this many frames before the shot
const FIND_SECS: f32 = 0.66; // full-frame candidate search window after the shot
const BORDER_FRAC: f32 = 0.12; // FIND phase ignores spots this close to the edge
const LOCAL_LAG: usize = 4; // frames back for the short-lag comparison
const MAX_CANDS: usize = 250; // strongest spots kept per frame
const MIN_TRACK_LEN: usize = 6; // FIND detections required for a valid track
const MIN_TRAVEL: f32 = 15.0; // px, start to end of FIND track
const MIN_STRAIGHTNESS: f32 = 0.6; // net displacement / path length (FIND)
const FIND_MAX_GAP: usize = 2; // FIND frames a track may coast
const FIND_MAX_LEN: usize = 16;
const LATE_MAX_SPEEDUP: f32 = 1.3; // a late track speeding up more than this is falling // FIND detections before FOLLOW takes over
const EARLY_FRAMES: usize = 6; // detections used to measure launch direction
const STATIC_RADIUS: usize = 3; // px search for the spot in a reference
const SHIFT_COARSE: i32 = 10; // ±px search at 1/4 scale (±40 px full-res)
const SHIFT_FINE: i32 = 3; // ±px full-res refinement
const FIT_POINTS: usize = 8; // recent points used for the curved prediction
const INNER: usize = 1; // 3x3 centre box
const RING_IN: usize = 4; // ring between 9x9 ...
const RING_OUT: usize = 9; // ... and 19x19 boxes

/// Every tunable of the BB tracker. `sensitivity` is a convenience that sets
/// the `follow_*` fields through [`TrackParams::with_sensitivity`].
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TrackParams {
    /// Draw the highlight disc on the BB.
    pub highlight: bool,
    /// 0 = only follow strong, nearby spots (may lose the BB),
    /// 1 = follow faint spots over a wide area (may wander onto clutter).
    pub sensitivity: f32,
    /// Seconds after the shot to keep following the BB.
    pub max_flight_secs: f32,
    /// FIND phase: minimum spot contrast (centre minus surround, 8-bit luma).
    pub find_contrast: f32,
    /// FIND phase: reject spots this visible (x contrast) in a reference.
    pub find_static_ratio: f32,
    /// FIND phase: max surrounding change (x contrast) vs a reference.
    pub find_ring_change: f32,
    /// Seconds after the shot within which the BB must first be seen.
    pub find_seed_secs: f32,
    /// If no BB leaves the launch point, accept one first seen anywhere up to
    /// this long after the shot (hidden behind the foreground at first).
    /// 0 = off.
    pub find_late_secs: f32,
    /// Detections a late-found track needs (stricter than a launch track).
    pub find_late_min_detections: u32,
    /// A late-found track must start within this distance of the launch
    /// point (fraction of frame width).
    pub find_late_radius: f32,
    /// FOLLOW phase: minimum spot contrast.
    pub follow_contrast: f32,
    /// FOLLOW phase: base search radius around the prediction, px.
    pub follow_radius: f32,
    /// FOLLOW phase: extra search radius per px/frame of BB speed.
    pub follow_radius_per_speed: f32,
    /// FOLLOW phase: frames the BB may be invisible before giving up.
    pub follow_max_gap: u32,
    /// FOLLOW phase: largest change in the BB's on-screen velocity between
    /// frames, px/frame². Sudden changes mean an impact or a jump to clutter.
    pub follow_max_accel: f32,
    /// FOLLOW phase: a spot may be at most this many times brighter (contrast)
    /// than the recent BB detections; the BB only fades with distance.
    pub follow_max_brightening: f32,
    /// FOLLOW phase: a spot must keep at least this fraction of the recent
    /// BB contrast (a sudden drop means a different, fainter object).
    pub follow_min_fade: f32,
    /// FOLLOW phase: on-screen speed may grow to this multiple of the speed
    /// when following began (plus 3 px/frame).
    pub follow_max_speed_gain: f32,
    /// FOLLOW phase: after the BB is missed, the spot that resumes the track
    /// may be at most this many times as bright as the last detection.
    pub follow_reacquire_brightening: f32,
    /// FOLLOW phase: after a miss, the search radius is scaled by this.
    pub follow_reacquire_radius: f32,
    /// FOLLOW phase: a spot must also be new compared with the frame from
    /// when the BB was this many px back along its path (rejects moving
    /// players and hands, which were not there before the shot).
    pub follow_novelty_px: f32,
    /// FOLLOW phase: reject spots this visible (x contrast) before the shot.
    pub follow_static_ratio: f32,
    /// FOLLOW phase: max surrounding change (x contrast) vs before the shot.
    pub follow_ring_change: f32,
    /// Where the BB first appears, as a fraction of frame width/height.
    pub launch_x: f32,
    pub launch_y: f32,
    /// How far from the launch point (fraction of frame width) a track may start.
    pub launch_radius: f32,
    /// Direction of the BB's first movement, degrees in image space
    /// (0 = right, -90 = up, 90 = down).
    pub launch_dir_deg: f32,
    /// Allowed deviation from `launch_dir_deg`, degrees.
    pub launch_dir_tol_deg: f32,
    /// Impact if the BB suddenly slows to this fraction of its speed (it hit
    /// something and stopped). 0 = off.
    pub impact_stop_ratio: f32,
    /// ... provided it was moving at least this fast before (px/frame);
    /// distant BBs slow gradually and are not affected.
    pub impact_stop_min_speed: f32,
    /// ... and its speed after is at most this (px/frame): it really stopped,
    /// rather than slowing as it heads away from the camera.
    pub impact_stop_max_speed: f32,
    /// Impact if the BB's brightness suddenly steps down to this fraction and
    /// stays there (it hit something). 0 = off.
    pub impact_dim_ratio: f32,
    /// Impact if the BB vanishes while its brightness is at least this
    /// multiple of `follow_contrast`. Lower = more impacts detected.
    pub impact_vanish_ratio: f32,
    /// Impact if the scene around the BB's last position changes by at least
    /// this many luma levels just after it vanishes. Lower = more impacts.
    pub impact_change: f32,
    /// Highlight disc diameter as a percentage of the BB's measured diameter
    /// (200 = twice the size).
    pub highlight_size_pct: f32,
    /// Smallest highlight disc radius, px.
    pub highlight_min_radius: f32,
    /// Highlight colour, sRGB.
    pub highlight_colour: [u8; 3],
}

impl Default for TrackParams {
    fn default() -> Self {
        TrackParams {
            highlight: true,
            sensitivity: 0.5,
            max_flight_secs: 2.0,
            find_contrast: 18.0,
            find_static_ratio: 0.6,
            find_ring_change: 0.25,
            find_seed_secs: 0.33,
            find_late_secs: 1.0,
            find_late_min_detections: 10,
            find_late_radius: 0.12,
            follow_contrast: 0.0,
            follow_radius: 0.0,
            follow_radius_per_speed: 0.0,
            follow_max_gap: 0,
            follow_max_accel: 0.0,
            follow_max_brightening: 0.0,
            follow_min_fade: 0.0,
            follow_novelty_px: 14.0,
            follow_reacquire_brightening: 0.0,
            follow_reacquire_radius: 0.0,
            follow_max_speed_gain: 0.0,
            follow_static_ratio: 0.0,
            follow_ring_change: 0.0,
            launch_x: 0.468,
            launch_y: 0.465,
            launch_radius: 0.055,
            launch_dir_deg: -90.0,
            launch_dir_tol_deg: 45.0,
            impact_stop_ratio: 0.35,
            impact_stop_min_speed: 2.0,
            impact_stop_max_speed: 0.6,
            impact_dim_ratio: 0.6,
            impact_vanish_ratio: 4.0,
            impact_change: 8.0,
            highlight_size_pct: 200.0,
            highlight_min_radius: 3.0,
            highlight_colour: [255, 255, 255],
        }
        .with_sensitivity(0.5)
    }
}

impl TrackParams {
    /// Set `sensitivity` and derive every FOLLOW-phase threshold from it.
    pub fn with_sensitivity(mut self, s: f32) -> Self {
        let s = s.clamp(0.0, 1.0);
        let lerp = |a: f32, b: f32| a + (b - a) * s;
        self.sensitivity = s;
        self.follow_contrast = lerp(16.0, 5.0);
        self.follow_radius = lerp(5.0, 22.0);
        self.follow_radius_per_speed = lerp(0.25, 0.8);
        self.follow_max_gap = lerp(1.0, 8.0).round() as u32;
        self.follow_max_accel = lerp(1.5, 6.0);
        self.follow_max_brightening = lerp(1.4, 3.0);
        self.follow_min_fade = lerp(0.5, 0.15);
        self.follow_reacquire_brightening = lerp(1.2, 1.8);
        self.follow_reacquire_radius = lerp(0.4, 0.8);
        self.follow_max_speed_gain = lerp(1.5, 5.0);
        self.follow_static_ratio = lerp(0.5, 0.85);
        self.follow_ring_change = lerp(0.25, 0.6);
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
}

/// One highlight disc on one source frame.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub x: f32,
    pub y: f32,
    pub r: f32, // drawn radius (already scaled)
}

/// Marks keyed by absolute source frame number.
pub type Marks = HashMap<i64, Mark>;

/// Result of tracking one shot.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub marks: Marks,
    /// Frame-space position of the first detection, and the launch direction
    /// (degrees) over the first detections. Used when learning a profile.
    pub launch: Option<(f32, f32, f32)>,
    /// Detections in the FIND and FOLLOW phases.
    pub found: usize,
    pub followed: usize,
    /// Where and when the BB hit something, if an impact was detected.
    pub impact: Option<Impact>,
}

impl Track {
    /// True if `self` follows the same BB as `other` (an earlier shot): on
    /// at least 3 shared frames their marks are typically within 10 px.
    pub fn same_bb_as(&self, other: &Track) -> bool {
        let mut d: Vec<f32> = self
            .marks
            .iter()
            .filter_map(|(f, m)| other.marks.get(f).map(|o| (m.x - o.x).hypot(m.y - o.y)))
            .collect();
        if d.len() < 3 {
            return false;
        }
        d.sort_by(f32::total_cmp);
        d[d.len() / 2] <= 10.0
    }

    /// Seconds of BB flight observed: from the shot (the BB is in flight
    /// from the moment of firing, even before it can be seen) to the last
    /// frame with a BB mark. 0 if no BB was found.
    pub fn flight_secs(&self, shot_time: f32, fps: f64) -> f32 {
        match self.marks.keys().max() {
            Some(&b) => ((b + 1) as f64 / fps) as f32 - shot_time,
            None => 0.0,
        }
    }
}

/// A detected BB impact, in frame coordinates.
#[derive(Clone, Debug)]
pub struct Impact {
    pub frame: i64,
    pub x: f32,
    pub y: f32,
    /// How far past the detection thresholds the impact was (≥ 1).
    pub strength: f32,
    /// Where the impact point is on later frames as the camera moves.
    pub path: HashMap<i64, (f32, f32)>,
}

#[derive(Clone, Copy, Debug)]
struct Spot {
    x: f32,
    y: f32,
    contrast: f32,
    radius: f32,
}

pub fn probe_video(path: &Path) -> Result<VideoInfo> {
    let out = crate::pipeline::tool("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0", "-show_entries",
            "stream=width,height,r_frame_rate", "-of", "csv=p=0",
        ])
        .arg(path)
        .output()?;
    if !out.status.success() {
        bail!("ffprobe failed on video stream");
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = s.trim().split(',').collect();
    if f.len() < 3 {
        bail!("unexpected ffprobe output: {s}");
    }
    let (num, den) = f[2].split_once('/').unwrap_or((f[2], "1"));
    let fps = num.parse::<f64>()? / den.parse::<f64>()?.max(1.0);
    Ok(VideoInfo { width: f[0].parse()?, height: f[1].parse()?, fps })
}

/// Track the BB fired at `shot_time` (seconds). An empty `marks` means no
/// convincing trajectory was found.
pub fn track_shot(path: &Path, info: VideoInfo, shot_time: f32, p: &TrackParams) -> Result<Track> {
    track_shot_before(path, info, shot_time, None, p)
}

/// As [`track_shot`], where `next_shot` is the time of the next shot sound
/// in the same file: a BB first seen after it belongs to that shot, so the
/// late pick-up window stops there.
pub fn track_shot_before(
    path: &Path,
    info: VideoInfo,
    shot_time: f32,
    next_shot: Option<f32>,
    p: &TrackParams,
) -> Result<Track> {
    let debug = std::env::var_os("GHILLIECLIP_DEBUG").is_some();
    let shot_frame = (shot_time as f64 * info.fps).round() as i64;
    let first = (shot_frame - PRE_FRAMES).max(0);
    let n = PRE_FRAMES as usize + (p.max_flight_secs.max(0.2) as f64 * info.fps) as usize;
    let frames = decode_gray(path, info, first, n)?;
    let base = (shot_frame - first) as usize; // index of the shot frame
    if frames.len() <= base + MIN_TRACK_LEN {
        return Ok(Track::default());
    }
    let (w, h) = (info.width, info.height);
    let reference = contrast_map(&frames[0], w, h);
    let ref_small = downsample4(&frames[0], w, h);

    // Shake of every post-shot frame relative to the reference.
    let mut shifts = Vec::with_capacity(frames.len() - base);
    let mut shift = (0, 0);
    for f in &frames[base..] {
        shift = estimate_shift(&frames[0], &ref_small, f, w, h, shift).0;
        shifts.push(shift);
    }

    // FIND phase: full-frame candidates, checked against the pre-shot
    // reference and the frame LOCAL_LAG earlier (whose short-lag alignment
    // stays accurate during pans, while the BB has moved clear).
    let find_secs = FIND_SECS.max(p.find_late_secs + 0.3);
    let find_frames = ((find_secs as f64 * info.fps) as usize).min(frames.len() - base);
    let mut maps: HashMap<usize, Vec<f32>> = HashMap::new();
    for i in base.saturating_sub(LOCAL_LAG)..base {
        maps.insert(i, contrast_map(&frames[i], w, h));
    }
    let mut cands: Vec<Vec<Spot>> = Vec::with_capacity(find_frames);
    for k in 0..find_frames {
        let i = base + k;
        let f = &frames[i];
        let c = contrast_map(f, w, h);
        let mut refs = vec![RefView { img: &frames[0], contrast: Some(&reference), shift: shifts[k] }];
        let li = i.saturating_sub(LOCAL_LAG);
        if li > 0 {
            if let Some(lc) = maps.get(&li) {
                let lf = &frames[li];
                let (ls, _) = estimate_shift(lf, &downsample4(lf, w, h), f, w, h, (0, 0));
                refs.push(RefView { img: lf, contrast: Some(lc), shift: ls });
            }
        }
        cands.push(find_spots(f, &c, w, h, &refs, p));
        maps.remove(&li);
        maps.insert(i, c);
    }
    drop(maps);
    if debug {
        for (k, c) in cands.iter().enumerate() {
            let top: Vec<String> = c
                .iter()
                .take(4)
                .map(|s| format!("({:.0},{:.0})c{:.0}r{:.1}", s.x, s.y, s.contrast, s.radius))
                .collect();
            eprintln!("  find k={k:2} shift={:?} n={} {}", shifts[k], c.len(), top.join(" "));
        }
    }

    let seed_frames = ((p.find_seed_secs as f64 * info.fps).round() as usize).max(2);
    // (x, y, launch radius, late pick-up radius): a late BB is already near
    // the aim point, which is close to the launch point in the view.
    let launch =
        (p.launch_x * w as f32, p.launch_y * h as f32, p.launch_radius * w as f32, p.find_late_radius * w as f32);
    let mut late_secs = p.find_late_secs;
    if let Some(n) = next_shot {
        late_secs = late_secs.min(n - shot_time);
    }
    let late_frames = (late_secs.max(0.0) as f64 * info.fps).round() as usize;
    let launched = best_track(&cands, launch, seed_frames, p, false);
    // Fallback: the BB may only become visible later (hidden behind the
    // foreground at first) and away from the launch point.
    let late = launched.is_none() && late_frames > seed_frames;
    let Some(mut track) = launched.or_else(|| if late { best_track(&cands, launch, late_frames, p, true) } else { None })
    else {
        return Ok(Track::default());
    };
    if debug {
        eprintln!("  find: {} track of {} detection(s)", if late { "late" } else { "launch" }, track.len());
    }
    let found = track.len();

    // FOLLOW phase.
    let (followed, stop) = follow(&frames[base..], &frames[0], &reference, &shifts, w, h, &mut track, p, debug);
    // A late-found "BB" that speeds up over its whole length is something
    // falling (a BB dropping after a hit elsewhere, debris), not a shot's BB.
    if late && track.len() >= 6 {
        let third = track.len() / 3;
        let sp = |a: usize, b: usize| {
            let ((ka, sa), (kb, sb)) = (track[a], track[b]);
            (sb.x - sa.x).hypot(sb.y - sa.y) / (kb - ka).max(1) as f32
        };
        if sp(track.len() - 1 - third, track.len() - 1) > sp(0, third) * LATE_MAX_SPEEDUP {
            if debug {
                eprintln!("  late track speeds up: falling object, not a BB in flight");
            }
            return Ok(Track::default());
        }
    }

    // Impact. First: a BB that stops dead has hit something (e.g. a hanging
    // blanket, after which it may slide or fall); the track ends there.
    // Otherwise: it vanished while bright, or the scene around it changed.
    // A BB that suddenly dims and stays dim has hit something too (it is
    // then partly hidden in, or lying against, the target).
    let hit = match (abrupt_stop(&track, p), dim_step(&track, p)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let impact = match hit {
        Some(i) => {
            track.truncate(i + 1);
            let (k, sp) = track[i];
            Some((k, sp.x + shifts[k].0 as f32, sp.y + shifts[k].1 as f32, 1.0))
        }
        None if stop == Stop::Lost => detect_impact(&frames[base..], &frames[0], &shifts, w, h, &track, info.fps, p),
        None => None,
    };
    if debug {
        eprintln!("  impact: {impact:?}");
    }

    // Launch statistics in reference (pre-shot scene) coordinates; only
    // meaningful when the BB was seen leaving the launch point.
    let (s0, s1) = (track[0].1, track[EARLY_FRAMES.min(track.len()) - 1].1);
    let dir = (s1.y - s0.y).atan2(s1.x - s0.x).to_degrees();
    let launch_stat = (!late).then_some((s0.x, s0.y, dir));

    // Back from reference coordinates to each frame's own coordinates.
    for (k, s) in &mut track {
        s.x += shifts[*k].0 as f32;
        s.y += shifts[*k].1 as f32;
    }

    // Smooth the radius (median of 3; the BB never grows), scale it, and
    // fill gaps by interpolation.
    let mut marks = Marks::new();
    let mut r_prev = f32::MAX;
    for i in 0..track.len() {
        let (k, s) = track[i];
        let mut rr = [
            track[i.saturating_sub(1)].1.radius,
            s.radius,
            track[(i + 1).min(track.len() - 1)].1.radius,
        ];
        rr.sort_by(f32::total_cmp);
        let r_bb = rr[1].min(r_prev);
        r_prev = r_bb;
        let r = (r_bb * p.highlight_size_pct / 100.0).max(p.highlight_min_radius);
        marks.insert(shot_frame + k as i64, Mark { x: s.x, y: s.y, r });
        if let Some(&(k2, s2)) = track.get(i + 1) {
            for g in k + 1..k2 {
                let t = (g - k) as f32 / (k2 - k) as f32;
                marks.insert(
                    shot_frame + g as i64,
                    Mark { x: s.x + (s2.x - s.x) * t, y: s.y + (s2.y - s.y) * t, r },
                );
            }
        }
    }
    // The impact point stays fixed in the scene: follow the camera's
    // movement over the rest of the analysed frames.
    let impact = impact.map(|(k, x, y, strength)| {
        let (s0x, s0y) = shifts[k.min(shifts.len() - 1)];
        let (rx, ry) = (x - s0x as f32, y - s0y as f32);
        let path = (k..shifts.len())
            .map(|kk| (shot_frame + kk as i64, (rx + shifts[kk].0 as f32, ry + shifts[kk].1 as f32)))
            .collect();
        Impact { frame: shot_frame + k as i64, x, y, strength, path }
    });
    Ok(Track { marks, launch: launch_stat, found, followed, impact })
}

fn decode_gray(path: &Path, info: VideoInfo, first: i64, n: usize) -> Result<Vec<Vec<u8>>> {
    let ss = format!("{:.6}", (first as f64 - 0.5).max(0.0) / info.fps);
    let mut child = crate::pipeline::tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-ss", &ss, "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-frames:v", &n.to_string(), "-pix_fmt", "gray", "-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning ffmpeg for BB tracking")?;
    let mut out = child.stdout.take().unwrap();
    let mut frames = Vec::with_capacity(n);
    loop {
        let mut f = vec![0u8; info.width * info.height];
        if out.read_exact(&mut f).is_err() {
            break;
        }
        frames.push(f);
    }
    let _ = child.wait();
    Ok(frames)
}

/// Summed-area table with a one-pixel zero border.
fn integral(img: &[u8], w: usize, h: usize) -> Vec<u32> {
    let mut s = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += img[y * w + x] as u32;
            s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
        }
    }
    s
}

#[inline]
fn box_sum(s: &[u32], w: usize, x: usize, y: usize, r: usize) -> u32 {
    let (x0, y0, x1, y1) = (x - r, y - r, x + r + 1, y + r + 1);
    let w1 = w + 1;
    s[y1 * w1 + x1] + s[y0 * w1 + x0] - s[y0 * w1 + x1] - s[y1 * w1 + x0]
}

#[inline]
fn spot_contrast(s: &[u32], w: usize, x: usize, y: usize) -> f32 {
    let n_in = ((2 * INNER + 1) * (2 * INNER + 1)) as f32;
    let n_mid = (2 * RING_IN + 1) * (2 * RING_IN + 1);
    let n_ring = ((2 * RING_OUT + 1) * (2 * RING_OUT + 1) - n_mid) as f32;
    let inner = box_sum(s, w, x, y, INNER) as f32 / n_in;
    let ring = (box_sum(s, w, x, y, RING_OUT) - box_sum(s, w, x, y, RING_IN)) as f32 / n_ring;
    inner - ring
}

/// Point-spot contrast: mean of 3x3 centre minus mean of the surrounding ring.
fn contrast_map(img: &[u8], w: usize, h: usize) -> Vec<f32> {
    let s = integral(img, w, h);
    let mut c = vec![0f32; w * h];
    for y in RING_OUT..h - RING_OUT {
        for x in RING_OUT..w - RING_OUT {
            c[y * w + x] = spot_contrast(&s, w, x, y);
        }
    }
    c
}

fn downsample4(img: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (sw, sh) = (w / 4, h / 4);
    let mut out = vec![0u8; sw * sh];
    for y in 0..sh {
        for x in 0..sw {
            let mut s = 0u32;
            for yy in 0..4 {
                for xx in 0..4 {
                    s += img[(4 * y + yy) * w + 4 * x + xx] as u32;
                }
            }
            out[y * sw + x] = (s / 16) as u8;
        }
    }
    out
}

/// Sum of absolute differences between `cur` and `refr` shifted by (dx, dy),
/// i.e. comparing cur(x, y) with refr(x - dx, y - dy), over the central area.
fn sad(refr: &[u8], cur: &[u8], w: usize, h: usize, dx: i32, dy: i32, step: usize) -> u64 {
    let (mx, my) = (w / 5, h / 5);
    let mut s = 0u64;
    for y in (my..h - my).step_by(step) {
        let ry = y as i32 - dy;
        if ry < 0 || ry >= h as i32 {
            return u64::MAX;
        }
        for x in (mx..w - mx).step_by(step) {
            let rx = x as i32 - dx;
            if rx < 0 || rx >= w as i32 {
                return u64::MAX;
            }
            s += (cur[y * w + x] as i32 - refr[ry as usize * w + rx as usize] as i32).unsigned_abs() as u64;
        }
    }
    s
}

/// Translation (dx, dy) such that frame(x, y) ≈ reference(x - dx, y - dy),
/// and the mean absolute luma residual after applying it.
/// Coarse search at quarter resolution around `prev`, then full-res refine.
fn estimate_shift(
    refr: &[u8],
    ref_small: &[u8],
    cur: &[u8],
    w: usize,
    h: usize,
    prev: (i32, i32),
) -> ((i32, i32), f32) {
    let small = downsample4(cur, w, h);
    let (sw, sh) = (w / 4, h / 4);
    let (pcx, pcy) = (prev.0 / 4, prev.1 / 4);
    let mut best = (u64::MAX, pcx, pcy);
    for dy in pcy - SHIFT_COARSE..=pcy + SHIFT_COARSE {
        for dx in pcx - SHIFT_COARSE..=pcx + SHIFT_COARSE {
            let v = sad(ref_small, &small, sw, sh, dx, dy, 2);
            if v < best.0 {
                best = (v, dx, dy);
            }
        }
    }
    let (cx, cy) = (best.1 * 4, best.2 * 4);
    let mut best = (u64::MAX, cx, cy);
    for dy in cy - SHIFT_FINE..=cy + SHIFT_FINE {
        for dx in cx - SHIFT_FINE..=cx + SHIFT_FINE {
            let v = sad(refr, cur, w, h, dx, dy, 4);
            if v < best.0 {
                best = (v, dx, dy);
            }
        }
    }
    let samples = ((h - 2 * (h / 5)).div_ceil(4) * (w - 2 * (w / 5)).div_ceil(4)) as f32;
    ((best.1, best.2), best.0 as f32 / samples)
}

/// A frame to compare against, and the shift mapping the current frame to it.
struct RefView<'a> {
    img: &'a [u8],
    /// Precomputed contrast map of `img`, or None to compute it on demand.
    contrast: Option<&'a [f32]>,
    shift: (i32, i32),
}

/// Spot contrast of `img` at one pixel, without a precomputed map.
fn contrast_at(img: &[u8], w: usize, h: usize, x: usize, y: usize) -> f32 {
    if x < RING_OUT || y < RING_OUT || x + RING_OUT >= w || y + RING_OUT >= h {
        return 0.0;
    }
    let (mut inner, mut ring) = (0u32, 0u32);
    let (ri, ro) = (RING_IN as i32, RING_OUT as i32);
    for dy in -ro..=ro {
        let row = (y as i32 + dy) as usize * w;
        for dx in -ro..=ro {
            let v = img[row + (x as i32 + dx) as usize] as u32;
            if dx.abs() <= INNER as i32 && dy.abs() <= INNER as i32 {
                inner += v;
            }
            if dx.abs() > ri || dy.abs() > ri {
                ring += v;
            }
        }
    }
    let n_in = ((2 * INNER + 1) * (2 * INNER + 1)) as f32;
    let n_ring = ((2 * RING_OUT + 1).pow(2) - (2 * RING_IN + 1).pow(2)) as f32;
    inner as f32 / n_in - ring as f32 / n_ring
}

/// FIND phase: bright spots in `img` (contrast map `c`) that are absent from
/// every view in `refs`. Coordinates are returned in the space of `refs[0]`.
fn find_spots(img: &[u8], c: &[f32], w: usize, h: usize, refs: &[RefView], p: &TrackParams) -> Vec<Spot> {
    // The BB starts near the reticle; the frame edges are mostly foreground
    // clutter (leaves, scope housing) that defeats the reference.
    let mx = ((w as f32 * BORDER_FRAC) as usize).max(RING_OUT + 2);
    let my = ((h as f32 * BORDER_FRAC) as usize).max(RING_OUT + 2);
    let mut spots = Vec::new();
    for y in my..h - my {
        for x in mx..w - mx {
            let v = c[y * w + x];
            if v < p.find_contrast || !is_local_max(c, w, x, y, v) {
                continue;
            }
            if !refs.iter().all(|r| is_new(img, w, h, x, y, v, r, p.find_static_ratio, p.find_ring_change)) {
                continue;
            }
            let mut spot = refine(img, w, h, x, y, v);
            spot.x -= refs[0].shift.0 as f32;
            spot.y -= refs[0].shift.1 as f32;
            spots.push(spot);
        }
    }
    spots.sort_by(|a, b| b.contrast.total_cmp(&a.contrast));
    spots.truncate(MAX_CANDS);
    spots
}

/// Strict local maximum in 5x5 (ties broken by position).
fn is_local_max(c: &[f32], w: usize, x: usize, y: usize, v: f32) -> bool {
    for yy in y - 2..=y + 2 {
        for xx in x - 2..=x + 2 {
            let u = c[yy * w + xx];
            if u > v || (u == v && (yy, xx) < (y, x)) {
                return false;
            }
        }
    }
    true
}

/// True if the spot at (x, y) with contrast `v` is absent from `r`
/// (`static_ratio`), and its surroundings are unchanged there
/// (`ring_ratio`): a BB is an isolated dot over static background, while
/// parts of a moving player or swaying foliage change their surroundings too.
#[allow(clippy::too_many_arguments)]
fn is_new(
    img: &[u8],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    v: f32,
    r: &RefView,
    static_ratio: f32,
    ring_ratio: f32,
) -> bool {
    let (rx, ry) = (x as i32 - r.shift.0, y as i32 - r.shift.1);
    // Area revealed by the shake has no valid reference to compare.
    let edge = (RING_OUT + STATIC_RADIUS) as i32;
    if rx < edge || ry < edge || rx >= w as i32 - edge || ry >= h as i32 - edge {
        return false;
    }
    let sr = STATIC_RADIUS as i32;
    let mut ref_max = f32::MIN;
    for yy in ry - sr..=ry + sr {
        for xx in rx - sr..=rx + sr {
            let c = match r.contrast {
                Some(map) => map[yy as usize * w + xx as usize],
                None => contrast_at(r.img, w, h, xx as usize, yy as usize),
            };
            ref_max = ref_max.max(c);
        }
    }
    ref_max < v * static_ratio && ring_change(img, r.img, w, h, x, y, r.shift) <= v * ring_ratio
}

/// Mean absolute luma change, frame vs shake-compensated reference, over the
/// ring around (x, y) that excludes the spot itself.
fn ring_change(img: &[u8], refr: &[u8], w: usize, h: usize, x: usize, y: usize, shift: (i32, i32)) -> f32 {
    let (ro, ri) = (RING_OUT as i32, RING_IN as i32);
    let (mut sum, mut n) = (0u32, 0u32);
    for dy in -ro..=ro {
        for dx in -ro..=ro {
            if dx.abs() <= ri && dy.abs() <= ri {
                continue;
            }
            let (fx, fy) = (x as i32 + dx, y as i32 + dy);
            let (rx, ry) = (fx - shift.0, fy - shift.1);
            if fx < 0 || fy < 0 || fx >= w as i32 || fy >= h as i32 {
                continue;
            }
            if rx < 0 || ry < 0 || rx >= w as i32 || ry >= h as i32 {
                continue;
            }
            let a = img[fy as usize * w + fx as usize] as i32;
            let b = refr[ry as usize * w + rx as usize] as i32;
            sum += (a - b).unsigned_abs();
            n += 1;
        }
    }
    if n == 0 { f32::MAX } else { sum as f32 / n as f32 }
}

/// How much brighter the 3x3 mean at (x, y) is than the brightest 3x3 mean
/// within ±1 px of the same place in the shake-compensated `refr`.
fn brightening(img: &[u8], refr: &[u8], w: usize, h: usize, x: usize, y: usize, shift: (i32, i32)) -> f32 {
    let mean3 = |im: &[u8], cx: i32, cy: i32| -> Option<f32> {
        if cx < 1 || cy < 1 || cx + 1 >= w as i32 || cy + 1 >= h as i32 {
            return None;
        }
        let mut s = 0u32;
        for dy in -1..=1 {
            for dx in -1..=1 {
                s += im[(cy + dy) as usize * w + (cx + dx) as usize] as u32;
            }
        }
        Some(s as f32 / 9.0)
    };
    let Some(here) = mean3(img, x as i32, y as i32) else { return 0.0 };
    let (rx, ry) = (x as i32 - shift.0, y as i32 - shift.1);
    let mut before = f32::MIN;
    for dy in -1..=1 {
        for dx in -1..=1 {
            match mean3(refr, rx + dx, ry + dy) {
                Some(m) => before = before.max(m),
                None => return 0.0,
            }
        }
    }
    here - before
}

/// Sub-pixel centroid and radius from pixels above half the spot's contrast.
fn refine(img: &[u8], w: usize, h: usize, x: usize, y: usize, contrast: f32) -> Spot {
    const R: usize = 7;
    let peak = img[y * w + x] as f32;
    let bg = peak - contrast;
    let thr = bg + contrast * 0.5;
    let (mut n, mut sx, mut sy, mut sw) = (0usize, 0f32, 0f32, 0f32);
    for yy in y.saturating_sub(R)..=(y + R).min(h - 1) {
        for xx in x.saturating_sub(R)..=(x + R).min(w - 1) {
            let v = img[yy * w + xx] as f32;
            if v >= thr {
                let wt = v - bg;
                n += 1;
                sx += xx as f32 * wt;
                sy += yy as f32 * wt;
                sw += wt;
            }
        }
    }
    let (cx, cy) = if sw > 0.0 { (sx / sw, sy / sw) } else { (x as f32, y as f32) };
    let radius = ((n.max(1) as f32) / std::f32::consts::PI).sqrt();
    Spot { x: cx, y: cy, contrast, radius }
}

/// Angular difference in degrees, folded to [0, 180].
fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// FIND phase: strongest smooth trajectory, as (frame offset from shot, spot)
/// pairs in reference coordinates. Normally it must start within
/// `seed_frames` at the launch point and leave in the launch direction; a
/// `late` track may start anywhere within `seed_frames` but must be longer.
fn best_track(
    cands: &[Vec<Spot>],
    launch: (f32, f32, f32, f32),
    seed_frames: usize,
    p: &TrackParams,
    late: bool,
) -> Option<Vec<(usize, Spot)>> {
    let min_len = if late { p.find_late_min_detections.max(MIN_TRACK_LEN as u32) as usize } else { MIN_TRACK_LEN };
    let mut best: Option<(f32, Vec<(usize, Spot)>)> = None;
    for start in 1..seed_frames.min(cands.len()) {
        for &seed in &cands[start] {
            // Seeds and the launch point are both in reference (pre-shot
            // scene) coordinates: the BB leaves where the bore pointed when
            // fired, however the camera moves afterwards.
            let from_launch = (seed.x - launch.0).hypot(seed.y - launch.1);
            if (!late && from_launch > launch.2) || (late && from_launch > launch.3) {
                continue;
            }
            let track = extend(cands, start, seed);
            if track.len() < min_len {
                continue;
            }
            let early = track[EARLY_FRAMES.min(track.len()) - 1].1;
            let (edx, edy) = (early.x - track[0].1.x, early.y - track[0].1.y);
            if !late
                && (edx.hypot(edy) < 1.0
                    || angle_diff(edy.atan2(edx).to_degrees(), p.launch_dir_deg) > p.launch_dir_tol_deg)
            {
                continue;
            }
            let (a, b) = (track[0].1, track[track.len() - 1].1);
            let net = (a.x - b.x).hypot(a.y - b.y);
            let path: f32 = track.windows(2).map(|q| (q[1].1.x - q[0].1.x).hypot(q[1].1.y - q[0].1.y)).sum();
            // A real BB travels in a near-straight line; clutter that
            // slipped through (leaves, residual shake) wobbles in place.
            if net < MIN_TRAVEL || net < path * MIN_STRAIGHTNESS {
                continue;
            }
            // A late track that speeds up is something falling (a BB dropping
            // after a hit, debris), not a BB in flight, which slows down.
            if late {
                let half = track.len() / 2;
                let sp = |a: usize, b: usize| {
                    let ((ka, sa), (kb, sb)) = (track[a], track[b]);
                    (sb.x - sa.x).hypot(sb.y - sa.y) / (kb - ka).max(1) as f32
                };
                if sp(half, track.len() - 1) > sp(0, half) * LATE_MAX_SPEEDUP {
                    continue;
                }
            }
            let score: f32 = track.iter().map(|t| t.1.contrast).sum();
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, track));
            }
        }
    }
    best.map(|(_, mut t)| {
        extend_back(cands, &mut t);
        t
    })
}

/// Extend a FIND track backwards in time to the BB's first visible frame.
fn extend_back(cands: &[Vec<Spot>], track: &mut Vec<(usize, Spot)>) {
    while track.len() >= 2 {
        let ((k0, s0), (k1, s1)) = (track[0], track[1]);
        let dt = (k1 - k0) as f32;
        let (vx, vy) = ((s1.x - s0.x) / dt, (s1.y - s0.y) / dt);
        let speed = vx.hypot(vy);
        let mut found = None;
        for step in 1..=FIND_MAX_GAP + 1 {
            let Some(f) = k0.checked_sub(step) else { break };
            if f == 0 {
                break;
            }
            let (px, py) = (s0.x - vx * step as f32, s0.y - vy * step as f32);
            let tol = (0.4 * speed + 5.0) * step as f32;
            let pick = cands[f]
                .iter()
                .map(|s| (s, (s.x - px).hypot(s.y - py)))
                .filter(|(_, d)| *d <= tol)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((s, _)) = pick {
                found = Some((f, *s));
                break;
            }
        }
        match found {
            Some(t) => track.insert(0, t),
            None => break,
        }
    }
}

fn extend(cands: &[Vec<Spot>], start: usize, seed: Spot) -> Vec<(usize, Spot)> {
    let mut track = vec![(start, seed)];
    // The FIND phase only establishes the track; the FOLLOW phase, with its
    // brightness and re-acquisition checks, takes it from there.
    while track.len() < FIND_MAX_LEN {
        let (lk, last) = *track.last().unwrap();
        let (vx, vy) = match track.len() {
            1 => (0.0, 0.0),
            n => {
                let (pk, prev) = track[n - 2];
                let dt = (lk - pk) as f32;
                ((last.x - prev.x) / dt, (last.y - prev.y) / dt)
            }
        };
        let mut found = None;
        for step in 1..=FIND_MAX_GAP + 1 {
            let f = lk + step;
            if f >= cands.len() {
                break;
            }
            let (px, py) = (last.x + vx * step as f32, last.y + vy * step as f32);
            let speed = vx.hypot(vy);
            let tol = if track.len() == 1 { 60.0 } else { (0.4 * speed + 5.0) * step as f32 };
            // Early on, apparent speed only falls as the BB flies away.
            let max_step = if track.len() == 1 { f32::MAX } else { (speed * 1.3).max(speed + 3.0) };
            let pick = cands[f]
                .iter()
                .map(|s| (s, (s.x - px).hypot(s.y - py)))
                .filter(|(_, d)| *d <= tol)
                .filter(|(s, _)| (s.x - last.x).hypot(s.y - last.y) / step as f32 <= max_step)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((s, _)) = pick {
                found = Some((f, *s));
                break;
            }
        }
        match found {
            Some(t) => track.push(t),
            None => break,
        }
    }
    track
}

/// Least-squares quadratic through the last points of `track`, evaluated at
/// frame `k`: (x, y, vx, vy). Curvature is capped at `max_accel`
/// px/frame². Falls back to linear with fewer than four points.
fn predict(track: &[(usize, Spot)], k: usize, max_accel: f32) -> (f32, f32, f32, f32) {
    let pts = &track[track.len().saturating_sub(FIT_POINTS)..];
    let (k0, _) = *pts.last().unwrap();
    let ts: Vec<f64> = pts.iter().map(|(kk, _)| *kk as f64 - k0 as f64).collect();
    let t = k as f64 - k0 as f64;
    let fit = |vals: Vec<f64>| -> (f64, f64) {
        // Returns (value at t, derivative at t).
        if pts.len() >= 4 {
            let (mut s0, mut s1, mut s2, mut s3, mut s4) = (0.0, 0.0, 0.0, 0.0, 0.0);
            let (mut b0, mut b1, mut b2) = (0.0, 0.0, 0.0);
            for (ti, vi) in ts.iter().zip(&vals) {
                let t2 = ti * ti;
                s0 += 1.0;
                s1 += ti;
                s2 += t2;
                s3 += t2 * ti;
                s4 += t2 * t2;
                b0 += vi;
                b1 += vi * ti;
                b2 += vi * t2;
            }
            // Solve [s0 s1 s2; s1 s2 s3; s2 s3 s4] [a b c] = [b0 b1 b2].
            let m = [[s0, s1, s2, b0], [s1, s2, s3, b1], [s2, s3, s4, b2]];
            if let Some([a, b, c]) = solve3(m) {
                let c = c.clamp(-max_accel as f64 / 2.0, max_accel as f64 / 2.0);
                return (a + b * t + c * t * t, b + 2.0 * c * t);
            }
        }
        let n = vals.len();
        if n >= 2 {
            let dt = ts[n - 1] - ts[n - 2];
            let v = (vals[n - 1] - vals[n - 2]) / dt.max(1.0);
            (vals[n - 1] + v * t, v)
        } else {
            (vals[n - 1], 0.0)
        }
    };
    let (x, vx) = fit(pts.iter().map(|(_, s)| s.x as f64).collect());
    let (y, vy) = fit(pts.iter().map(|(_, s)| s.y as f64).collect());
    (x as f32, y as f32, vx as f32, vy as f32)
}

fn solve3(mut m: [[f64; 4]; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let piv = (col..3).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[piv][col].abs() < 1e-9 {
            return None;
        }
        m.swap(col, piv);
        for row in 0..3 {
            if row != col {
                let f = m[row][col] / m[col][col];
                for k in col..4 {
                    m[row][k] -= f * m[col][k];
                }
            }
        }
    }
    Some([m[0][3] / m[0][0], m[1][3] / m[1][1], m[2][3] / m[2][2]])
}

/// Why the FOLLOW phase stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stop {
    /// The BB vanished (no acceptable spot for `follow_max_gap` frames).
    Lost,
    /// The BB's predicted path left the frame.
    LeftFrame,
    /// The analysis window (`max_flight_secs`) ended.
    Timeout,
}

/// FOLLOW phase: extend `track` (reference coordinates) through the remaining
/// frames with a guided search around the curved prediction. Returns the
/// number of detections added and why it stopped.
#[allow(clippy::too_many_arguments)]
fn follow(
    post: &[Vec<u8>],
    ref_img: &[u8],
    ref_contrast: &[f32],
    shifts: &[(i32, i32)],
    w: usize,
    h: usize,
    track: &mut Vec<(usize, Spot)>,
    p: &TrackParams,
    debug: bool,
) -> (usize, Stop) {
    let mut added = 0;
    let mut k = track.last().unwrap().0 + 1;
    let start_speed = {
        let (_, _, vx, vy) = predict(track, k - 1, p.follow_max_accel);
        vx.hypot(vy)
    };
    let max_speed = start_speed * p.follow_max_speed_gain + 3.0;
    let mut misses = 0u32;
    while k < post.len() {
        let (px, py, _, _) = predict(track, k, p.follow_max_accel);
        let (lk, last) = *track.last().unwrap();
        let (_, _, lvx, lvy) = predict(track, lk, p.follow_max_accel);
        let speed = lvx.hypot(lvy);
        let gap = (k - lk) as f32;
        let mut radius = p.follow_radius + p.follow_radius_per_speed * speed * gap.sqrt();
        // Picking the BB up again after a miss: it must be close to where it
        // should be (after an impact, nearby clutter is otherwise grabbed).
        if gap > 1.0 {
            radius = (radius * p.follow_reacquire_radius).max(4.0);
        }
        let shift = shifts[k];
        // Prediction in this frame's own coordinates.
        let (fx, fy) = (px + shift.0 as f32, py + shift.1 as f32);
        let margin = (RING_OUT + STATIC_RADIUS + 2) as f32;
        if fx < margin || fy < margin || fx >= w as f32 - margin || fy >= h as f32 - margin {
            return (added, Stop::LeftFrame);
        }
        // New relative to before the shot, and relative to a recent frame far
        // enough back that the BB has moved clear: rejects moving players,
        // hands and foliage that were not there before the shot.
        let mut rvs = vec![RefView { img: ref_img, contrast: Some(ref_contrast), shift }];
        let lag = ((p.follow_novelty_px / speed.max(0.1)).ceil() as usize).clamp(3, 30);
        // Only compare surroundings with the recent frame if the BB has moved
        // clear of the surround ring since then (else it sees itself).
        let local_ring_ok = speed * lag as f32 >= (RING_OUT + 2) as f32;
        if k > lag {
            let li = k - lag;
            let local = (shift.0 - shifts[li].0, shift.1 - shifts[li].1);
            rvs.push(RefView { img: &post[li], contrast: None, shift: local });
        }
        let mut recent: Vec<f32> = track.iter().rev().take(3).map(|t| t.1.contrast).collect();
        recent.sort_by(f32::total_cmp);
        let typical = recent[recent.len() / 2];
        let (mut max_contrast, min_contrast) = (typical * p.follow_max_brightening, typical * p.follow_min_fade);
        // Picking the BB up again after a miss: it should be about as bright
        // as just before it was lost (a fading BB that vanished on impact
        // must not be replaced by a brighter object nearby).
        if gap > 1.0 {
            max_contrast = max_contrast.min(last.contrast * p.follow_reacquire_brightening);
        }
        // Velocity this spot implies must stay within the acceleration and
        // speed limits, and its brightness close to the recent BB's.
        let ok = |s: &Spot| {
            let (vx, vy) = ((s.x - shift.0 as f32 - last.x) / gap, (s.y - shift.1 as f32 - last.y) / gap);
            (vx - lvx).hypot(vy - lvy) <= p.follow_max_accel * gap
                && vx.hypot(vy) <= max_speed
                && (min_contrast..=max_contrast).contains(&s.contrast)
        };
        let pick = search_near(&post[k], w, h, fx, fy, radius, p, &rvs, local_ring_ok, &ok);
        if debug {
            eprintln!(
                "  follow k={k:3} pred=({px:.0},{py:.0}) speed={speed:.1} radius={radius:.0} -> {}",
                pick.map_or("miss".into(), |s| format!("({:.0},{:.0}) c{:.0}", s.x - shift.0 as f32, s.y - shift.1 as f32, s.contrast))
            );
        }
        match pick {
            Some(mut s) => {
                s.x -= shift.0 as f32;
                s.y -= shift.1 as f32;
                track.push((k, s));
                added += 1;
                misses = 0;
            }
            None => {
                misses += 1;
                if misses > p.follow_max_gap {
                    return (added, Stop::Lost);
                }
            }
        }
        k += 1;
    }
    (added, Stop::Timeout)
}

/// Best new spot within `radius` of (cx, cy) in frame coordinates, scored by
/// contrast with a Gaussian penalty on distance from the prediction.
#[allow(clippy::too_many_arguments)]
fn search_near(
    img: &[u8],
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    radius: f32,
    p: &TrackParams,
    rvs: &[RefView],
    local_ring_ok: bool,
    accept: &dyn Fn(&Spot) -> bool,
) -> Option<Spot> {
    let pad = RING_OUT + 2;
    let r = radius.ceil() as usize;
    let x0 = (cx as usize).saturating_sub(r + pad).max(0);
    let y0 = (cy as usize).saturating_sub(r + pad).max(0);
    let x1 = (cx as usize + r + pad).min(w - 1);
    let y1 = (cy as usize + r + pad).min(h - 1);
    let (ww, wh) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut win = vec![0u8; ww * wh];
    for y in 0..wh {
        win[y * ww..(y + 1) * ww].copy_from_slice(&img[(y0 + y) * w + x0..(y0 + y) * w + x1 + 1]);
    }
    let s = integral(&win, ww, wh);
    let mut c = vec![f32::MIN; ww * wh];
    for y in RING_OUT..wh - RING_OUT {
        for x in RING_OUT..ww - RING_OUT {
            c[y * ww + x] = spot_contrast(&s, ww, x, y);
        }
    }
    let mut best: Option<(f32, Spot)> = None;
    for y in pad..wh.saturating_sub(pad) {
        for x in pad..ww.saturating_sub(pad) {
            let v = c[y * ww + x];
            if v < p.follow_contrast || !is_local_max(&c, ww, x, y, v) {
                continue;
            }
            let (gx, gy) = (x0 + x, y0 + y);
            let d = (gx as f32 - cx).hypot(gy as f32 - cy);
            if d > radius {
                continue;
            }
            // Against the pre-shot frame only ask "was a bright spot already
            // here?": its surroundings may legitimately differ (players move).
            // Surroundings are compared with the recent frame instead.
            // "Did this spot get brighter?" rather than "was there already a
            // bright spot nearby?": a faint BB over bright texture still
            // brightens its own pixels, while unchanged scenery does not.
            let need = v * (1.0 - p.follow_static_ratio).max(0.05);
            let new: Vec<bool> = rvs
                .iter()
                .enumerate()
                .map(|(i, rv)| {
                    let ring = match i {
                        0 if rvs.len() > 1 => f32::MAX,
                        1 if !local_ring_ok => f32::MAX,
                        _ => p.follow_ring_change,
                    };
                    brightening(img, rv.img, w, h, gx, gy, rv.shift) >= need
                        && ring_change(img, rv.img, w, h, gx, gy, rv.shift) <= v * ring
                })
                .collect();
            if std::env::var_os("GHILLIECLIP_DEBUG2").is_some() {
                let sp = refine(img, w, h, gx, gy, v);
                eprintln!("      cand ({gx},{gy}) c{v:.0} d{d:.0} new={new:?} accept={}", accept(&sp));
            }
            if new.iter().any(|n| !n) {
                continue;
            }
            let score = v * (-(d / radius).powi(2)).exp();
            if best.as_ref().is_none_or(|(b, _)| score > *b) {
                let spot = refine(img, w, h, gx, gy, v);
                if accept(&spot) {
                    best = Some((score, spot));
                }
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Index in `track` of the last bright detection before the BB's brightness
/// steps down: the median of the next DIM_AFTER (at least DIM_MIN_AFTER)
/// detections is at most `impact_dim_ratio` x the median of the previous
/// DIM_BEFORE. Gradual fading with distance does not trigger it.
fn dim_step(track: &[(usize, Spot)], p: &TrackParams) -> Option<usize> {
    const DIM_BEFORE: usize = 6;
    const DIM_AFTER: usize = 6;
    const DIM_MIN_AFTER: usize = 4;
    if p.impact_dim_ratio <= 0.0 || track.len() < DIM_BEFORE + DIM_MIN_AFTER {
        return None;
    }
    let med = |a: usize, b: usize| {
        let mut v: Vec<f32> = track[a..b].iter().map(|t| t.1.contrast).collect();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    (DIM_BEFORE..=track.len() - DIM_MIN_AFTER).find_map(|i| {
        let after = med(i, (i + DIM_AFTER).min(track.len()));
        // The step itself: the first dim detection is already dim.
        (after <= med(i - DIM_BEFORE, i) * p.impact_dim_ratio
            && track[i].1.contrast <= med(i - DIM_BEFORE, i) * p.impact_dim_ratio.max(0.75))
        .then_some(i - 1)
    })
}

/// Index in `track` where the BB stops dead: its speed over the previous
/// STOP_WINDOW detections is at least `impact_stop_min_speed` and over the
/// next STOP_WINDOW at most `impact_stop_ratio` of that, on two consecutive
/// detections.
fn abrupt_stop(track: &[(usize, Spot)], p: &TrackParams) -> Option<usize> {
    const STOP_WINDOW: usize = 4;
    if p.impact_stop_ratio <= 0.0 || track.len() < 2 * STOP_WINDOW + 1 {
        return None;
    }
    let speed = |a: usize, b: usize| {
        let ((ka, sa), (kb, sb)) = (track[a], track[b]);
        (sb.x - sa.x).hypot(sb.y - sa.y) / (kb - ka).max(1) as f32
    };
    if std::env::var_os("GHILLIECLIP_DEBUG").is_some() {
        let vel = |a: usize, b: usize| {
            let ((ka, sa), (kb, sb)) = (track[a], track[b]);
            let dt = (kb - ka).max(1) as f32;
            ((sb.x - sa.x) / dt, (sb.y - sa.y) / dt)
        };
        for i in STOP_WINDOW..track.len() - STOP_WINDOW {
            let (b, a) = (vel(i - STOP_WINDOW, i), vel(i, i + STOP_WINDOW));
            eprintln!(
                "  stop? k={} before={:.2} after={:.2} dv={:.2}",
                track[i].0,
                speed(i - STOP_WINDOW, i),
                speed(i, i + STOP_WINDOW),
                (a.0 - b.0).hypot(a.1 - b.1)
            );
        }
    }
    let stopped = |i: usize| {
        let before = speed(i - STOP_WINDOW, i);
        let after = speed(i, i + STOP_WINDOW);
        before >= p.impact_stop_min_speed && after <= before * p.impact_stop_ratio && after <= p.impact_stop_max_speed
    };
    // The stop must hold on two consecutive windows: a single slow window at
    // the end of a track is noise, a BB that hit something stays put.
    (STOP_WINDOW..track.len() - STOP_WINDOW - 1).find(|&i| stopped(i) && stopped(i + 1))
}

/// Mean absolute luma change between `img` and the shake-compensated
/// reference over a square of half-size `half` centred on (cx, cy) in
/// reference coordinates.
#[allow(clippy::too_many_arguments)]
fn local_change(img: &[u8], refr: &[u8], shift: (i32, i32), w: usize, h: usize, cx: f32, cy: f32, half: i32) -> f32 {
    let (mut sum, mut n) = (0u64, 0u64);
    for dy in -half..=half {
        for dx in -half..=half {
            let (rx, ry) = (cx as i32 + dx, cy as i32 + dy);
            let (fx, fy) = (rx + shift.0, ry + shift.1);
            if rx < 0 || ry < 0 || fx < 0 || fy < 0 || rx >= w as i32 || fx >= w as i32 || ry >= h as i32 || fy >= h as i32 {
                continue;
            }
            sum += (img[fy as usize * w + fx as usize] as i32 - refr[ry as usize * w + rx as usize] as i32).unsigned_abs() as u64;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum as f32 / n as f32 }
}

/// Detect a BB impact at the end of a track that stopped because the BB
/// vanished. It counts as an impact if the BB was still bright when it
/// vanished (`impact_vanish_ratio` x the follow threshold), or the scene around
/// its last position changes by more than `impact_change` luma levels within
/// ~100 ms. Returns (frame offset, frame-space x, y, strength), where
/// strength is the larger of the two cues relative to their thresholds.
#[allow(clippy::too_many_arguments)]
fn detect_impact(
    post: &[Vec<u8>],
    ref_img: &[u8],
    shifts: &[(i32, i32)],
    w: usize,
    h: usize,
    track: &[(usize, Spot)],
    fps: f64,
    p: &TrackParams,
) -> Option<(usize, f32, f32, f32)> {
    const HALF: i32 = 24;
    let (lk, last) = *track.last()?;
    let after = ((0.1 * fps).round() as usize).max(2); // look ~100 ms ahead
    if lk + after >= post.len() || track.len() < 4 {
        return None; // track ran to the end of the window: no evidence
    }
    let before: Vec<f32> = (lk.saturating_sub(3)..=lk)
        .map(|k| local_change(&post[k], ref_img, shifts[k], w, h, last.x, last.y, HALF))
        .collect();
    let base = before.iter().sum::<f32>() / before.len() as f32;
    let (kmax, peak) = (lk + 1..=lk + after)
        .map(|k| (k, local_change(&post[k], ref_img, shifts[k], w, h, last.x, last.y, HALF)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let change = peak - base;
    let mut recent: Vec<f32> = track.iter().rev().take(3).map(|t| t.1.contrast).collect();
    recent.sort_by(f32::total_cmp);
    let vanish = recent[recent.len() / 2] / p.follow_contrast.max(1.0);
    let strength = (change / p.impact_change.max(0.1)).max(vanish / p.impact_vanish_ratio.max(0.1));
    if std::env::var_os("GHILLIECLIP_DEBUG").is_some() {
        eprintln!("  impact test: change={change:.1} (k={kmax}) vanish={vanish:.1} strength={strength:.2}");
    }
    if strength < 1.0 {
        return None;
    }
    let (sx, sy) = shifts[lk];
    Some((lk + 1, last.x + sx as f32, last.y + sy as f32, strength))
}
