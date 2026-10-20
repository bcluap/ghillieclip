//! Learn a gun/camera profile from footage, with no prior knowledge.
//!
//! 1. Collect every loud transient in every file.
//! 2. Group them into clusters of mutually similar sounds of similar
//!    loudness. With the camera on the gun, the gun's own sounds (shot, and
//!    bolt/cocking for some guns) are the loudest repeated ones.
//! 3. For a sample of each cluster, look for a BB leaving the gun in the
//!    video. The cluster whose sounds launch BBs is the shot.
//! 4. Set detection thresholds from the spread of that cluster, with the
//!    high-frequency limit placed between the shot and the other clusters.
//! 5. Track a sample of detected shots with an open launch zone and take the
//!    densest group of BB start points and directions as the launch geometry.

use crate::audio::{self, Signature, Transient, quantile};
use crate::bb::{self, TrackParams};
use crate::pipeline::{Reporter, parallel_map};
use crate::profile::{Launch, Profile};
use crate::settings::{DetectParams, Settings};
use anyhow::{Result, bail};
use std::path::PathBuf;

const POOL: usize = 800; // loudest transients considered for clustering
const CLUSTER_SIM: f32 = 0.90; // similarity for two sounds to be "the same"
const CLUSTER_LEVEL: f32 = 2.0; // max loudness ratio within a cluster
const CLUSTER_HF: f32 = 0.12; // max difference in high-frequency share (shot vs bolt)
const MAX_CLUSTERS: usize = 4;
const SAMPLES_PER_CLUSTER: usize = 12;
const LAUNCH_SAMPLES: usize = 40;
const LEARN_FLIGHT_SECS: f32 = 0.7; // only the FIND phase matters here

struct Event {
    file: usize,
    t: Transient,
}

/// Tracker settings for discovery: BB may start anywhere near the centre and
/// leave in any direction.
fn open_params(base: &TrackParams) -> TrackParams {
    TrackParams {
        launch_x: 0.5,
        launch_y: 0.5,
        launch_radius: 0.3,
        launch_dir_tol_deg: 180.0,
        max_flight_secs: LEARN_FLIGHT_SECS,
        find_late_secs: 0.0,
        ..*base
    }
}

pub fn learn(files: &[PathBuf], s: &Settings, rep: &dyn Reporter) -> Result<Profile> {
    // 1. Every loud transient.
    let mut events: Vec<Event> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        if rep.cancelled() {
            bail!("cancelled");
        }
        rep.progress("Learning: listening", i, files.len());
        match audio::decode_audio(f) {
            Ok(a) => {
                let loudest = a.iter().fold(0f32, |m, v| m.max(v.abs()));
                let floor = (loudest * 0.1).max(0.01);
                let ts = audio::transients(&a, s.detect.onset_ratio, floor);
                rep.log(&format!("  {}: {} loud sound(s)", name(f), ts.len()));
                events.extend(ts.into_iter().map(|t| Event { file: i, t }));
            }
            Err(e) => rep.log(&format!("  {}: skipped ({e:#})", name(f))),
        }
    }
    if events.len() < 5 {
        bail!("not enough loud sounds in the footage to learn from ({} found)", events.len());
    }

    // 2. Clusters among the loudest.
    let mut pool: Vec<usize> = (0..events.len()).collect();
    pool.sort_by(|&a, &b| events[b].t.sig.peak.total_cmp(&events[a].t.sig.peak));
    pool.truncate(POOL);
    let same = |a: &Signature, b: &Signature| {
        audio::similarity(a, b) >= CLUSTER_SIM
            && (a.peak / b.peak).max(b.peak / a.peak) <= CLUSTER_LEVEL
            && (a.hf_ratio - b.hf_ratio).abs() <= CLUSTER_HF
    };
    let mut free = pool.clone();
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let min_size = (pool.len() / 100).max(4);
    while clusters.len() < MAX_CLUSTERS && !free.is_empty() {
        let best = free
            .iter()
            .map(|&i| (i, free.iter().filter(|&&j| same(&events[i].t.sig, &events[j].t.sig)).count()))
            .max_by_key(|&(_, n)| n)
            .unwrap();
        if best.1 < min_size {
            break;
        }
        let seed = events[best.0].t.sig.clone();
        let members: Vec<usize> = free.iter().copied().filter(|&j| same(&seed, &events[j].t.sig)).collect();
        free.retain(|j| !members.contains(j));
        clusters.push(members);
    }
    if clusters.is_empty() {
        bail!("no repeated loud sound found; is the camera mounted on the gun?");
    }
    for (c, m) in clusters.iter().enumerate() {
        let sigs: Vec<&Signature> = m.iter().map(|&i| &events[i].t.sig).collect();
        let t = audio::mean_signature(&sigs);
        rep.log(&format!("  sound group {}: {} event(s), level {:.3}, HF {:.2}", c + 1, m.len(), t.peak, t.hf_ratio));
    }

    // 3. Which cluster launches BBs? After the shot, the BB always starts at
    // the same point in the view; after a bolt/cocking sound a BB may still
    // be visible, but somewhere along its flight. So score each cluster by
    // how many sampled BB tracks start in one tight group.
    let open = open_params(&s.track);
    let threads = s.encode.threads.max(1);
    let mut scores = Vec::new();
    for (c, m) in clusters.iter().enumerate() {
        if rep.cancelled() {
            bail!("cancelled");
        }
        rep.progress("Learning: looking for BBs", c, clusters.len());
        let sample = spread(m, SAMPLES_PER_CLUSTER, |&i| (events[i].file, events[i].t.time));
        let obs: Vec<(f32, f32, f32)> = parallel_map(&sample, threads, |&i| {
            let e = &events[i];
            bb::probe_video(&files[e.file])
                .and_then(|info| {
                    let t = bb::track_shot(&files[e.file], info, e.t.time, &open)?;
                    Ok(t.launch.map(|(x, y, d)| (x / info.width as f32, y / info.height as f32, d)))
                })
                .ok()
                .flatten()
        })
        .into_iter()
        .flatten()
        .collect();
        let grouped = densest_group(&obs).len();
        rep.log(&format!(
            "  sound group {}: BB seen after {}/{} sampled, {grouped} starting from one point",
            c + 1,
            obs.len(),
            sample.len()
        ));
        scores.push(grouped);
    }
    // A group whose sounds nearly always come shortly after another group's
    // (and not the other way round) is a follow-on sound: bolt, cocking or
    // echo. It cannot be the shot.
    let preceded = |b: usize, a: usize| {
        let n = clusters[b]
            .iter()
            .filter(|&&j| {
                clusters[a].iter().any(|&i| {
                    events[i].file == events[j].file && (0.05..1.5).contains(&(events[j].t.time - events[i].t.time))
                })
            })
            .count();
        n as f32 / clusters[b].len().max(1) as f32
    };
    let follower: Vec<bool> = (0..clusters.len())
        .map(|b| {
            (0..clusters.len()).any(|a| {
                a != b && scores[a] > 0 && {
                    let (ba, ab) = (preceded(b, a), preceded(a, b));
                    ba >= 0.6 && ba >= ab + 0.3
                }
            })
        })
        .collect();
    for (c, f) in follower.iter().enumerate() {
        if *f {
            rep.log(&format!("  sound group {}: follows another sound (bolt/cocking/echo)", c + 1));
        }
    }
    // With the camera on the gun, its shot is one of the loudest repeated
    // sounds: other players' guns and footsteps are much quieter.
    let level: Vec<f32> = clusters
        .iter()
        .map(|m| {
            let mut p: Vec<f32> = m.iter().map(|&i| events[i].t.sig.peak).collect();
            audio::median(&mut p)
        })
        .collect();
    let loudest = level.iter().cloned().fold(0f32, f32::max);
    let chosen = (0..clusters.len())
        .filter(|&c| !follower[c] && level[c] >= loudest * 0.5)
        .max_by(|&a, &b| scores[a].cmp(&scores[b]).then(clusters[a].len().cmp(&clusters[b].len())))
        .unwrap_or(0);
    if scores[chosen] == 0 {
        rep.log("  warning: no BBs seen in any sound group; using the most frequent loud sound as the shot");
    }
    rep.log(&format!("  shot = sound group {}", chosen + 1));
    let members = &clusters[chosen];
    let sigs: Vec<&Signature> = members.iter().map(|&i| &events[i].t.sig).collect();
    let template = audio::mean_signature(&sigs);

    // 4. Thresholds from the cluster's spread.
    let mut sims: Vec<f32> = sigs.iter().map(|g| audio::similarity(g, &template)).collect();
    let mut levels: Vec<f32> = sigs.iter().map(|g| g.peak / template.peak).collect();
    let mut hfs: Vec<f32> = sigs.iter().map(|g| g.hf_ratio).collect();
    let threshold = (quantile(&mut sims, 0.05) - 0.05).clamp(0.75, 0.95);
    let min_rel_level = (quantile(&mut levels, 0.05) * 0.8).clamp(0.15, 0.8);
    let shot_hf = quantile(&mut hfs, 0.95);
    let mut max_hf = shot_hf + 0.05;
    for (c, m) in clusters.iter().enumerate() {
        if c == chosen {
            continue;
        }
        let mut other: Vec<f32> = m.iter().map(|&i| events[i].t.sig.hf_ratio).collect();
        let lo = quantile(&mut other, 0.25);
        if lo > shot_hf {
            max_hf = max_hf.min((shot_hf + lo) / 2.0);
        }
    }
    let max_hf = max_hf.clamp(0.1, 0.95);
    // Loudness cut-off: the gun's own shots form a loud group; other players'
    // guns are quieter. Put the cut-off in the valley between them if there
    // is one, else just below the shot group.
    let matching: Vec<f32> = events
        .iter()
        .map(|e| &e.t.sig)
        .filter(|g| audio::similarity(g, &template) >= threshold && g.hf_ratio <= max_hf)
        .map(|g| g.peak / template.peak)
        .collect();
    let min_rel_level = loudness_valley(&matching).unwrap_or(min_rel_level);
    let detect = DetectParams { threshold, min_rel_level, max_hf, ..s.detect };

    // 5. Launch geometry from a sample of detected shots.
    let shots: Vec<usize> = (0..events.len())
        .filter(|&i| {
            let g = &events[i].t.sig;
            audio::similarity(g, &template) >= detect.threshold
                && g.peak / template.peak >= detect.min_rel_level
                && g.hf_ratio <= detect.max_hf
        })
        .collect();
    rep.log(&format!("  {} shot(s) match the learned sound", shots.len()));
    let sample = spread(&shots, LAUNCH_SAMPLES, |&i| (events[i].file, events[i].t.time));
    let done = std::sync::atomic::AtomicUsize::new(0);
    let launches: Vec<(f32, f32, f32)> = parallel_map(&sample, threads, |&i| {
        if rep.cancelled() {
            return None;
        }
        let e = &events[i];
        let r = bb::probe_video(&files[e.file]).and_then(|info| {
            let t = bb::track_shot(&files[e.file], info, e.t.time, &open)?;
            Ok(t.launch.map(|(x, y, d)| (x / info.width as f32, y / info.height as f32, d)))
        });
        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        rep.progress("Learning: BB launch point", n, sample.len());
        r.ok().flatten()
    })
    .into_iter()
    .flatten()
    .collect();
    if rep.cancelled() {
        bail!("cancelled");
    }
    let launch = densest_launch(&launches);
    match &launch {
        Some(l) => rep.log(&format!(
            "  BB launch point ({:.3}, {:.3}) ±{:.3}, direction {:.0}°±{:.0}° from {} track(s)",
            l.x, l.y, l.radius, l.dir_deg, l.dir_tol_deg, launches.len()
        )),
        None => rep.log("  not enough BB tracks to learn the launch point; keeping the current one"),
    }

    let first = name(&files[0]);
    let name = if files.len() == 1 {
        format!("Learned from {first}")
    } else {
        format!("Learned from {} files ({first} …)", files.len())
    };
    Ok(Profile { name, signature: template, detect, launch })
}

fn name(p: &std::path::Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Up to `n` items spread evenly through `items` ordered by `key`.
fn spread<T: Copy, K: PartialOrd>(items: &[T], n: usize, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut v = items.to_vec();
    v.sort_by(|a, b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal));
    if v.len() <= n {
        return v;
    }
    (0..n).map(|k| v[k * v.len() / n]).collect()
}

/// Cut-off at the emptiest point of the (log-spaced, smoothed) loudness
/// histogram between 0.2 and 0.8 x the typical shot, if it is a clear dip.
fn loudness_valley(levels: &[f32]) -> Option<f32> {
    const BINS: usize = 24;
    let (lo, hi) = (0.1f32.ln(), 1.5f32.ln());
    let mut h = [0f32; BINS];
    for &l in levels {
        let b = ((l.max(1e-3).ln() - lo) / (hi - lo) * BINS as f32).floor();
        if (0.0..BINS as f32).contains(&b) {
            h[b as usize] += 1.0;
        }
    }
    let sm: Vec<f32> = (0..BINS).map(|i| (h[i.saturating_sub(1)] + 2.0 * h[i] + h[(i + 1).min(BINS - 1)]) / 4.0).collect();
    let centre = |i: usize| (lo + (i as f32 + 0.5) / BINS as f32 * (hi - lo)).exp();
    let (vi, &vmin) = sm.iter().enumerate().filter(|(i, _)| (0.2..=0.8).contains(&centre(*i))).min_by(|a, b| a.1.total_cmp(b.1))?;
    let above = sm[vi..].iter().cloned().fold(0f32, f32::max);
    // A clear dip: well below the shot group's peak, with most of the
    // matching sounds above it.
    let louder = levels.iter().filter(|&&l| l >= centre(vi)).count();
    (vmin <= above * 0.35 && louder * 2 >= levels.len()).then(|| centre(vi))
}

/// Observations within 0.05 (fraction of frame) of the busiest one.
fn densest_group(obs: &[(f32, f32, f32)]) -> Vec<&(f32, f32, f32)> {
    const NEAR: f32 = 0.05;
    let near = |a: &(f32, f32, f32), b: &(f32, f32, f32)| (a.0 - b.0).hypot(a.1 - b.1) <= NEAR;
    let Some(centre) = obs.iter().max_by_key(|a| obs.iter().filter(|b| near(a, b)).count()) else {
        return Vec::new();
    };
    obs.iter().filter(|b| near(centre, b)).collect()
}

/// Launch geometry from the densest group of (x, y, direction) observations.
fn densest_launch(obs: &[(f32, f32, f32)]) -> Option<Launch> {
    let group = densest_group(obs);
    if group.len() < 3 {
        return None;
    }
    let mut xs: Vec<f32> = group.iter().map(|g| g.0).collect();
    let mut ys: Vec<f32> = group.iter().map(|g| g.1).collect();
    let (x, y) = (audio::median(&mut xs), audio::median(&mut ys));
    let mut dists: Vec<f32> = group.iter().map(|g| (g.0 - x).hypot(g.1 - y)).collect();
    let radius = (audio::median(&mut dists) * 3.0).clamp(0.03, 0.10);
    // Circular mean of directions, then their spread.
    let (sx, sy) = group.iter().fold((0f32, 0f32), |(sx, sy), g| {
        (sx + g.2.to_radians().cos(), sy + g.2.to_radians().sin())
    });
    let dir = sy.atan2(sx).to_degrees();
    let mut devs: Vec<f32> = group
        .iter()
        .map(|g| {
            let d = (g.2 - dir).rem_euclid(360.0);
            d.min(360.0 - d)
        })
        .collect();
    let tol = (audio::median(&mut devs) * 3.0).clamp(25.0, 60.0);
    Some(Launch { x, y, radius, dir_deg: dir, dir_tol_deg: tol })
}
