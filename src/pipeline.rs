//! The end-to-end job shared by the CLI and the GUI: resolve the profile,
//! detect shots, track BBs, draw overlays, encode and join the clips.

use crate::audio::{self, Detection};
use crate::bb::{self, Track, VideoInfo};
use crate::learn;
use crate::overlay::{self, Canvas};
use crate::profile::Profile;
use crate::settings::{Ballistics, ProfileSource, Settings};
use crate::builtin;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Progress, logging and cancellation for a running job.
pub trait Reporter: Sync {
    fn log(&self, msg: &str);
    fn progress(&self, stage: &str, done: usize, total: usize);
    fn cancelled(&self) -> bool;
    /// The current list of detected shots (sent again as tracking fills it in).
    fn shots(&self, _rows: &[ShotRow]) {}
}

/// One detected shot, for reporting.
#[derive(Clone, Debug)]
pub struct ShotRow {
    pub file: PathBuf,
    pub time: f32,
    pub score: f32,
    pub level: f32,
    /// Frames on which the BB was highlighted (None = not tracked yet/at all).
    pub bb_frames: Option<usize>,
    /// Distance at impact, metres, if an impact was detected.
    pub impact_m: Option<f64>,
}

/// What a run did.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub shots: Vec<ShotRow>,
    pub segments: usize,
    pub output: Option<PathBuf>,
    pub profile: Option<Profile>,
}

/// Map `f` over `items` on up to `threads` threads, preserving order.
pub fn parallel_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, items.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.expect("every item processed")).collect()
}

/// A `Command` for an FFmpeg tool. On Windows it runs without opening a
/// console window, so the GUI does not flash one per call.
pub fn tool(name: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(name);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

pub fn check_tools() -> Result<()> {
    for name in ["ffmpeg", "ffprobe"] {
        tool(name)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("`{name}` not found on PATH; install FFmpeg"))?;
    }
    Ok(())
}

const VIDEO_EXTS: [&str; 4] = ["mp4", "mov", "mkv", "m4v"];

/// Expand files and folders into a sorted list of videos, excluding `output`.
pub fn expand_inputs(inputs: &[PathBuf], output: &Path) -> Result<Vec<PathBuf>> {
    let out_canon = fs::canonicalize(output).ok();
    let is_video = |p: &Path| {
        p.is_file() && p.extension().is_some_and(|e| VIDEO_EXTS.iter().any(|v| e.eq_ignore_ascii_case(v)))
    };
    let mut files = Vec::new();
    for i in inputs {
        if i.is_dir() {
            let mut v: Vec<PathBuf> = fs::read_dir(i)
                .with_context(|| format!("reading {}", i.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| is_video(p))
                .collect();
            v.sort();
            files.extend(v);
        } else if is_video(i) {
            files.push(i.clone());
        } else {
            bail!("{} is not a video file or folder", i.display());
        }
    }
    files.retain(|p| out_canon.is_none() || fs::canonicalize(p).ok() != out_canon);
    files.dedup();
    Ok(files)
}

pub fn probe_duration(path: &Path) -> Result<f32> {
    let out = tool("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()?;
    if !out.status.success() {
        bail!("ffprobe failed");
    }
    String::from_utf8_lossy(&out.stdout).trim().parse::<f32>().context("unparseable duration")
}

/// Run ffmpeg, killing it if the job is cancelled.
fn run_ffmpeg<S: AsRef<OsStr>>(args: &[S], rep: &dyn Reporter) -> Result<()> {
    let mut child = tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning ffmpeg")?;
    let mut stderr = child.stderr.take().unwrap();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    loop {
        if let Some(status) = child.try_wait()? {
            let err = err_reader.join().unwrap_or_default();
            if !status.success() {
                bail!("ffmpeg failed: {}", err.trim());
            }
            return Ok(());
        }
        if rep.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("cancelled");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Resolve the profile named by `s.profile`, learning it if needed.
pub fn resolve_profile(s: &Settings, files: &[PathBuf], rep: &dyn Reporter) -> Result<Profile> {
    match &s.profile {
        ProfileSource::BuiltIn => Ok(builtin::profile()),
        ProfileSource::File(p) => Profile::load(p),
        ProfileSource::Learn => {
            rep.log(&format!("Learning the shot sound and BB launch from {} file(s)…", files.len()));
            learn::learn(files, s, rep)
        }
    }
}

/// A stretch of one source file to include, and the shots inside it.
struct Segment {
    file: PathBuf,
    start: f32,
    end: f32,
    shots: Vec<Detection>,
}

/// Turn shot times into non-overlapping [start, end] windows.
fn merge_windows(shots: &[Detection], pre: f32, post: f32, duration: f32) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    for s in shots {
        let (a, b) = ((s.time - pre).max(0.0), (s.time + post).min(duration));
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Run the whole job. `profile` is used as-is if given (its thresholds are
/// assumed to be in `s` already); otherwise it is resolved from `s.profile`
/// and applied to a copy of `s`.
pub fn run(s: &Settings, profile: Option<Profile>, dry_run: bool, rep: &dyn Reporter) -> Result<Summary> {
    check_tools()?;
    let files = expand_inputs(&s.inputs, &s.output)?;
    if files.is_empty() {
        bail!("no video files selected");
    }
    let mut s = s.clone();
    let profile = match profile {
        Some(p) => p,
        None => {
            let p = resolve_profile(&s, &files, rep)?;
            p.apply(&mut s);
            p
        }
    };
    rep.log(&format!("Profile: {}", profile.summary()));

    // Shot detection.
    let mut detected: Vec<(PathBuf, f32, Detection)> = Vec::new(); // (file, duration, shot)
    let mut rows: Vec<ShotRow> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        if rep.cancelled() {
            bail!("cancelled");
        }
        rep.progress("Detecting shots", i, files.len());
        let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let (duration, audio) = match probe_duration(file).and_then(|d| Ok((d, audio::decode_audio(file)?))) {
            Ok(v) => v,
            Err(e) => {
                rep.log(&format!("{name}: skipped ({e:#})"));
                continue;
            }
        };
        let shots = audio::detect(&audio, &profile.signature, &s.detect, |_| {});
        rep.log(&format!("{name}: {} shot sound(s)", shots.len()));
        for d in shots {
            rows.push(ShotRow {
                file: file.clone(),
                time: d.time,
                score: d.score,
                level: d.rel_level,
                bb_frames: None,
                impact_m: None,
            });
            detected.push((file.clone(), duration, d));
        }
        rep.shots(&rows);
    }
    rep.progress("Detecting shots", files.len(), files.len());

    // BB tracking, all shots in parallel. Also needed to confirm shots when
    // a minimum BB flight is required.
    let confirm = s.detect.require_bb_secs > 0.0;
    let mut rejected_sounds = 0usize;
    let mut tracks: HashMap<(PathBuf, u32), (Track, VideoInfo)> = HashMap::new();
    if wants_tracking(&s) || confirm {
        let jobs: Vec<(PathBuf, f32)> = detected.iter().map(|(f, _, d)| (f.clone(), d.time)).collect();
        // Next shot sound in the same file, which bounds a late BB pick-up.
        let next_of = |file: &PathBuf, t: f32| {
            detected.iter().filter(|(f, _, d)| f == file && d.time > t).map(|(_, _, d)| d.time).reduce(f32::min)
        };
        let done = AtomicUsize::new(0);
        let results = parallel_map(&jobs, s.encode.threads.max(1), |(file, t)| {
            if rep.cancelled() {
                return None;
            }
            let next = next_of(file, *t);
            let r = bb::probe_video(file)
                .and_then(|info| Ok((bb::track_shot_before(file, info, *t, next, &s.track)?, info)));
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            rep.progress("Tracking BBs", n, jobs.len());
            match r {
                Ok(v) => Some(v),
                Err(e) => {
                    rep.log(&format!("  BB tracking failed at {t:.2}s: {e:#}"));
                    None
                }
            }
        });
        if rep.cancelled() {
            bail!("cancelled");
        }
        for ((file, t), r) in jobs.into_iter().zip(results) {
            let Some((track, info)) = r else { continue };
            if let Some(row) = rows.iter_mut().find(|r| r.file == file && r.time == t) {
                row.bb_frames = Some(track.marks.len());
                row.impact_m = track.impact.as_ref().map(|im| s.ballistics.distance_m(im.frame as f64 / info.fps - t as f64));
            }
            tracks.insert((file, t.to_bits()), (track, info));
        }
    }

    // A sound whose BB track is the same BB as an earlier shot's (a bolt or
    // echo heard while that BB is still flying) is not a shot of its own.
    let mut dup: Vec<(PathBuf, f32, f32)> = Vec::new();
    for (f, _, d) in &detected {
        let Some((tr, _)) = tracks.get(&(f.clone(), d.time.to_bits())) else { continue };
        let earlier = detected.iter().filter(|(f2, _, d2)| f2 == f && d2.time < d.time).find(|(f2, _, d2)| {
            tracks.get(&(f2.clone(), d2.time.to_bits())).is_some_and(|(t2, _)| tr.same_bb_as(t2))
        });
        if let Some((_, _, e)) = earlier {
            dup.push((f.clone(), d.time, e.time));
        }
    }
    for (f, t, e) in &dup {
        let name = f.file_name().unwrap_or_default().to_string_lossy();
        rep.log(&format!("  {name} {t:.3}s: not a shot (same BB as the shot at {e:.3}s: bolt or echo)"));
        tracks.remove(&(f.clone(), t.to_bits()));
    }
    let is_dup = |file: &PathBuf, t: f32| dup.iter().any(|(f, d, _)| f == file && *d == t);
    detected.retain(|(f, _, d)| !is_dup(f, d.time));
    rows.retain(|r| !is_dup(&r.file, r.time));

    // A shot only counts if its BB was followed for long enough.
    if confirm {
        let before = detected.len();
        let keep = |file: &PathBuf, t: f32| {
            tracks.get(&(file.clone(), t.to_bits())).is_some_and(|(tr, info)| tr.flight_secs(t, info.fps) >= s.detect.require_bb_secs)
        };
        for (f, _, d) in detected.iter().filter(|(f, _, d)| !keep(f, d.time)) {
            let name = f.file_name().unwrap_or_default().to_string_lossy();
            rep.log(&format!(
                "  {name} {:.3}s: not a shot (no BB seen in flight {:.2}s after it)",
                d.time, s.detect.require_bb_secs
            ));
        }
        rejected_sounds = before - detected.iter().filter(|(f, _, d)| keep(f, d.time)).count();
        detected.retain(|(f, _, d)| keep(f, d.time));
        rows.retain(|r| keep(&r.file, r.time));
        rep.log(&format!("{} of {before} shot sound(s) confirmed by a BB in flight", detected.len()));
    }
    if wants_tracking(&s) {
        let tracked = rows.iter().filter(|r| r.bb_frames.is_some_and(|n| n > 0)).count();
        let impacts = rows.iter().filter(|r| r.impact_m.is_some()).count();
        rep.log(&format!("BB tracked on {tracked}/{} shot(s), {impacts} impact(s) detected", rows.len()));
    }
    rep.shots(&rows);

    // Clips.
    let mut segments: Vec<Segment> = Vec::new();
    for file in &files {
        let shots: Vec<Detection> = detected.iter().filter(|(f, _, _)| f == file).map(|(_, _, d)| d.clone()).collect();
        let Some(&(_, duration, _)) = detected.iter().find(|(f, _, _)| f == file) else { continue };
        for (start, end) in merge_windows(&shots, s.clips.pre_secs, s.clips.post_secs, duration) {
            let inside = shots.iter().filter(|d| d.time >= start && d.time <= end).cloned().collect();
            segments.push(Segment { file: file.clone(), start, end, shots: inside });
        }
    }
    rep.log(&format!("{} shot(s) across {} file(s) -> {} clip(s)", rows.len(), files.len(), segments.len()));
    let mut summary = Summary { shots: rows.clone(), segments: segments.len(), output: None, profile: Some(profile) };
    if dry_run {
        return Ok(summary);
    }
    if segments.is_empty() {
        let hint = if confirm && rejected_sounds > 0 {
            format!(
                ": {rejected_sounds} shot sound(s) had no BB seen in flight {:.2}s after them. Lower “Require BB in flight for” \
                 (0 = off) or raise the follow sensitivity",
                s.detect.require_bb_secs
            )
        } else {
            String::new()
        };
        bail!("no shots found, so there is nothing to write{hint}");
    }

    // Encode each clip, then join.
    let out_dir = match s.output.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    // Segments can total tens of GB at 2.7K, so stage them beside the output
    // rather than in /tmp (often a small RAM-backed tmpfs).
    let tmp = out_dir.join(format!(".ghillieclip-tmp-{}", std::process::id()));
    fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let result = (|| -> Result<()> {
        let mut list = String::new();
        for (i, seg) in segments.iter().enumerate() {
            if rep.cancelled() {
                bail!("cancelled");
            }
            rep.progress("Encoding clips", i, segments.len());
            let out = tmp.join(format!("seg_{i:04}.mp4"));
            let name = seg.file.file_name().unwrap_or_default().to_string_lossy();
            rep.log(&format!("Clip {}/{}: {name} {:.2}-{:.2}s", i + 1, segments.len(), seg.start, seg.end));
            let overlays = build_overlays(seg, &tracks, &s);
            if !s.encode.copy && !overlays.items.is_empty() {
                let info = tracks
                    .iter()
                    .find(|((f, _), _)| *f == seg.file)
                    .map(|(_, (_, info))| *info)
                    .map_or_else(|| bb::probe_video(&seg.file), Ok)?;
                render_segment(&seg.file, &out, info, seg.start, seg.end - seg.start, &overlays, &s, rep)?;
            } else {
                let mut a: Vec<String> = vec!["-ss".into(), format!("{:.3}", seg.start), "-i".into()];
                a.push(seg.file.to_string_lossy().into_owned());
                a.extend(["-t".into(), format!("{:.3}", seg.end - seg.start)]);
                if s.encode.copy {
                    a.extend(["-c", "copy", "-avoid_negative_ts", "make_zero"].map(String::from));
                } else {
                    a.extend(["-map", "0:v:0", "-map", "0:a:0?"].map(String::from));
                    a.extend(s.encode.args());
                }
                a.push(out.to_string_lossy().into_owned());
                run_ffmpeg(&a, rep)?;
            }
            // The concat demuxer resolves entries relative to list.txt, which
            // sits beside the segments, so use the bare file name.
            list.push_str(&format!("file 'seg_{i:04}.mp4'\n"));
        }
        rep.progress("Encoding clips", segments.len(), segments.len());
        let list_path = tmp.join("list.txt");
        fs::File::create(&list_path)?.write_all(list.as_bytes())?;
        let mut a: Vec<&OsStr> = ["-f", "concat", "-safe", "0", "-i"].map(OsStr::new).to_vec();
        a.push(list_path.as_os_str());
        a.extend(["-c", "copy", "-movflags", "+faststart"].map(OsStr::new));
        a.push(s.output.as_os_str());
        run_ffmpeg(&a, rep)?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&tmp);
    result?;
    rep.log(&format!("Wrote {}", s.output.display()));
    summary.output = Some(s.output.clone());
    Ok(summary)
}

fn wants_tracking(s: &Settings) -> bool {
    !s.encode.copy && (s.track.highlight || s.markers.enabled || s.distance.at_impact || s.distance.live)
}

/// Something drawn on one frame.
enum Item {
    Disc { x: f32, y: f32, r: f32 },
    Marker { x: f32, y: f32, t: f32, seed: u64 },
    Text { text: String, x: f32, y: f32, anchor: (f32, f32) },
}

struct Overlays {
    items: HashMap<i64, Vec<Item>>,
}

fn build_overlays(seg: &Segment, tracks: &HashMap<(PathBuf, u32), (Track, VideoInfo)>, s: &Settings) -> Overlays {
    let mut items: HashMap<i64, Vec<Item>> = HashMap::new();
    for d in &seg.shots {
        let Some((track, info)) = tracks.get(&(seg.file.clone(), d.time.to_bits())) else { continue };
        let fps = info.fps;
        // Time of flight is measured from the start of the shot sound.
        let tof = |frame: i64| frame as f64 / fps - d.time as f64;
        for (&f, m) in &track.marks {
            let v = items.entry(f).or_default();
            if s.track.highlight {
                v.push(Item::Disc { x: m.x, y: m.y, r: m.r });
            }
            if s.distance.live {
                v.push(Item::Text {
                    text: Ballistics::format(s.ballistics.distance_m(tof(f)), &s.distance),
                    x: m.x + m.r + s.distance.font_px * 0.3,
                    y: m.y,
                    anchor: (0.0, 0.5),
                });
            }
        }
        if let Some(im) = &track.impact {
            let frames = ((s.markers.duration_secs as f64 * fps).round() as i64).max(1);
            let seed = (im.x as u64) << 32 ^ im.y as u64 ^ im.frame as u64;
            let label = Ballistics::format(s.ballistics.distance_m(tof(im.frame)), &s.distance);
            let mut at = (im.x, im.y);
            for k in 0..frames {
                // Follow the camera while its movement is known.
                at = im.path.get(&(im.frame + k)).copied().unwrap_or(at);
                let v = items.entry(im.frame + k).or_default();
                if s.markers.enabled {
                    v.push(Item::Marker { x: at.0, y: at.1, t: k as f32 / frames as f32, seed });
                }
                if s.distance.at_impact {
                    let gap = if s.markers.enabled { s.markers.size_px * 0.75 } else { 12.0 };
                    v.push(Item::Text { text: label.clone(), x: at.0, y: at.1 - gap, anchor: (0.5, 1.0) });
                }
            }
        }
    }
    Overlays { items }
}

/// Re-encode `[start, start+dur)` of `src` into `dst`, painting `overlays`.
#[allow(clippy::too_many_arguments)]
fn render_segment(
    src: &Path,
    dst: &Path,
    info: VideoInfo,
    start: f32,
    dur: f32,
    overlays: &Overlays,
    s: &Settings,
    rep: &dyn Reporter,
) -> Result<()> {
    let (w, h) = (info.width, info.height);
    let first = (start as f64 * info.fps).round() as i64;
    let ss = format!("{:.6}", (first as f64 - 0.5).max(0.0) / info.fps);
    let nframes = (dur as f64 * info.fps).round() as usize;
    let size = format!("{w}x{h}");
    let rate = format!("{}", info.fps);
    let t = format!("{dur:.3}");

    let mut dec = tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-ss", &ss, "-i"])
        .arg(src)
        .args(["-map", "0:v:0", "-frames:v", &nframes.to_string(), "-pix_fmt", "yuv420p", "-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning ffmpeg decoder")?;
    let mut enc = tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &size, "-r", &rate])
        .args(["-color_range", "tv", "-colorspace", "bt709", "-i", "-", "-ss", &ss, "-t", &t, "-i"])
        .arg(src)
        .args(s.encode.args())
        .args(["-map", "0:v:0", "-map", "1:a:0?", "-shortest"])
        .arg(dst)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning ffmpeg encoder")?;

    let mut input = dec.stdout.take().unwrap();
    let mut output = enc.stdin.take().unwrap();
    let mut buf = vec![0u8; w * h * 3 / 2];
    let mut j = 0i64;
    let mut pipe_err = None;
    let mut cancelled = false;
    while input.read_exact(&mut buf).is_ok() {
        if rep.cancelled() {
            cancelled = true;
            break;
        }
        if let Some(list) = overlays.items.get(&(first + j)) {
            let mut c = Canvas::new(&mut buf, w, h);
            // Markers under discs under text.
            for it in list {
                if let Item::Marker { x, y, t, seed } = it {
                    overlay::marker(&mut c, s.markers.style, *x, *y, s.markers.size_px, s.markers.colour, *t, *seed);
                }
            }
            for it in list {
                if let Item::Disc { x, y, r } = it {
                    c.disc(*x, *y, *r, s.track.highlight_colour, 1.0);
                }
            }
            for it in list {
                if let Item::Text { text, x, y, anchor } = it {
                    c.text(text, *x, *y, *anchor, s.distance.font_px, s.distance.colour, 1.0);
                }
            }
        }
        if let Err(e) = output.write_all(&buf) {
            pipe_err = Some(e);
            break;
        }
        j += 1;
    }
    drop(output);
    drop(input);
    if cancelled {
        let _ = dec.kill();
        let _ = enc.kill();
        let _ = dec.wait();
        let _ = enc.wait();
        bail!("cancelled");
    }
    let dec_out = dec.wait_with_output()?;
    let enc_out = enc.wait_with_output()?;
    if !enc_out.status.success() {
        bail!("ffmpeg encode failed: {}", String::from_utf8_lossy(&enc_out.stderr).trim());
    }
    if pipe_err.is_none() && !dec_out.status.success() {
        bail!("ffmpeg decode failed: {}", String::from_utf8_lossy(&dec_out.stderr).trim());
    }
    if j == 0 {
        bail!("no frames decoded from {}", src.display());
    }
    Ok(())
}
