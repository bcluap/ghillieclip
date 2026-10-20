//! Shot detection from audio.
//!
//! Every sharp transient is fingerprinted (log-band spectrum of its first
//! ~21 ms plus an 80 ms decay envelope) and compared with the profile's shot
//! signature.

use crate::settings::DetectParams;
use anyhow::{Context, Result, bail};
use rustfft::{FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;

pub const SR: usize = 48_000;
const FFT_LEN: usize = 1024; // ~21 ms analysis window at the onset
const PRE_ROLL: usize = 64; // samples of window before the onset
pub const N_BANDS: usize = 40;
const BAND_LO: f32 = 150.0;
const BAND_HI: f32 = 20_000.0;
const ENV_HOP: usize = 96; // 2 ms envelope frames
pub const ENV_FRAMES: usize = 40; // 80 ms of decay envelope
const ONSET_HOP: usize = 48; // 1 ms onset-detection hop
const ONSET_GAP_S: f32 = 0.03; // minimum spacing between raw onsets
const HF_SPLIT: f32 = 4_000.0;
const START_LOOKBACK_S: f32 = 0.005; // how far back the true start may lie

/// Acoustic fingerprint of one transient.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Signature {
    /// Mean-centred log band energies.
    pub spectrum: Vec<f32>,
    /// Log RMS decay envelope, peak-normalised.
    pub envelope: Vec<f32>,
    /// Absolute peak amplitude.
    pub peak: f32,
    /// Share of 150 Hz-20 kHz energy above 4 kHz.
    pub hf_ratio: f32,
}

/// One transient in a file.
#[derive(Clone, Debug)]
pub struct Transient {
    /// Start of the sound, seconds: the firing time.
    pub time: f32,
    pub sig: Signature,
}

/// A transient accepted as a shot.
#[derive(Clone, Debug)]
pub struct Detection {
    /// Start of the shot sound, seconds.
    pub time: f32,
    pub score: f32,
    pub rel_level: f32,
}

/// Decode the first audio stream to mono 48 kHz f32 samples.
pub fn decode_audio(path: &Path) -> Result<Vec<f32>> {
    let mut child = crate::pipeline::tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-vn", "-ac", "1", "-ar", &SR.to_string(), "-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning ffmpeg")?;
    let mut bytes = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut bytes)?;
    if !child.wait()?.success() {
        bail!("ffmpeg audio decode failed (no audio stream or corrupt file?)");
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// All transients louder than `min_peak` whose energy rises by `ratio` above
/// the recent background, with their signatures.
pub fn transients(audio: &[f32], ratio: f32, min_peak: f32) -> Vec<Transient> {
    find_onsets(audio, ratio, min_peak)
        .into_iter()
        .filter_map(|o| {
            let sig = signature(audio, o)?;
            Some(Transient { time: sound_start(audio, o) as f32 / SR as f32, sig })
        })
        .collect()
}

/// Shots in `audio` matching `template`.
pub fn detect(
    audio: &[f32],
    template: &Signature,
    p: &DetectParams,
    mut verbose: impl FnMut(&str),
) -> Vec<Detection> {
    let min_peak = (template.peak * p.min_rel_level).max(0.005);
    let mut out: Vec<Detection> = Vec::new();
    for t in transients(audio, p.onset_ratio, min_peak) {
        let score = similarity(&t.sig, template);
        let rel_level = t.sig.peak / template.peak;
        verbose(&format!(
            "  candidate {:>8.3}s score={score:.3} level={rel_level:.2} hf={:.3}",
            t.time, t.sig.hf_ratio
        ));
        if score >= p.threshold && rel_level >= p.min_rel_level && t.sig.hf_ratio <= p.max_hf {
            let d = Detection { time: t.time, score, rel_level };
            match out.last_mut() {
                Some(prev) if d.time - prev.time < p.min_shot_gap => {
                    if d.score > prev.score {
                        *prev = d;
                    }
                }
                _ => out.push(d),
            }
        }
    }
    out
}

/// Sample indices where short-term energy jumps sharply above the recent
/// background. `ratio` is the required energy rise, `min_peak` an absolute
/// amplitude floor.
fn find_onsets(audio: &[f32], ratio: f32, min_peak: f32) -> Vec<usize> {
    let frames: Vec<f32> = audio
        .chunks(ONSET_HOP)
        .map(|c| c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32)
        .collect();
    let refractory = (ONSET_GAP_S * SR as f32) as usize / ONSET_HOP;
    let mut onsets = Vec::new();
    let mut last: Option<usize> = None;
    for i in 30..frames.len() {
        if last.is_some_and(|l| i - l < refractory) {
            continue;
        }
        let bg = frames[i - 30..i - 5].iter().sum::<f32>() / 25.0 + 1e-9;
        if frames[i] < bg * ratio {
            continue;
        }
        let start = i * ONSET_HOP;
        let end = (start + ONSET_HOP * 3).min(audio.len());
        let peak = audio[start..end].iter().fold(0f32, |m, v| m.max(v.abs()));
        if peak < min_peak {
            continue;
        }
        // Refine to the first sample reaching 30 % of the local peak.
        let thr = peak * 0.3;
        let s = (start..end).find(|&k| audio[k].abs() >= thr).unwrap_or(start);
        onsets.push(s);
        last = Some(i);
    }
    onsets
}

/// Walk back from an onset to where the sound first rises above the
/// background noise: the moment of firing.
fn sound_start(audio: &[f32], onset: usize) -> usize {
    let look = (START_LOOKBACK_S * SR as f32) as usize;
    let bg_from = onset.saturating_sub(look + 30 * ONSET_HOP);
    let bg_to = onset.saturating_sub(look);
    if bg_to <= bg_from {
        return onset;
    }
    let bg = (audio[bg_from..bg_to].iter().map(|v| v * v).sum::<f32>() / (bg_to - bg_from) as f32).sqrt();
    let thr = (bg * 4.0).max(1e-4);
    let mut s = onset;
    // Allow brief dips below the threshold (zero crossings) of up to 0.5 ms.
    let mut quiet = 0;
    let mut k = onset;
    while k > bg_to {
        k -= 1;
        if audio[k].abs() >= thr {
            s = k;
            quiet = 0;
        } else {
            quiet += 1;
            if quiet > SR / 2000 {
                break;
            }
        }
    }
    s
}

fn peak_at(audio: &[f32], onset: usize) -> f32 {
    let end = (onset + 5 * ONSET_HOP).min(audio.len());
    audio[onset..end].iter().fold(0f32, |m, v| m.max(v.abs()))
}

/// Fingerprint the transient starting at sample `onset`.
pub fn signature(audio: &[f32], onset: usize) -> Option<Signature> {
    let start = onset.checked_sub(PRE_ROLL)?;
    let env_len = ENV_HOP * ENV_FRAMES;
    if start + FFT_LEN.max(env_len + PRE_ROLL) > audio.len() {
        return None;
    }

    // Spectrum: Hann-windowed FFT -> log-spaced band energies.
    let mut buf: Vec<Complex<f32>> = (0..FFT_LEN)
        .map(|k| {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * k as f32 / FFT_LEN as f32).cos();
            Complex::new(audio[start + k] * w, 0.0)
        })
        .collect();
    thread_local! {
        static FFT: std::sync::Arc<dyn rustfft::Fft<f32>> =
            FftPlanner::new().plan_fft_forward(FFT_LEN);
    }
    FFT.with(|f| f.process(&mut buf));
    let mut bands = vec![1e-10f32; N_BANDS];
    let step = (BAND_HI / BAND_LO).ln() / N_BANDS as f32;
    for (k, c) in buf.iter().enumerate().take(FFT_LEN / 2).skip(1) {
        let f = k as f32 * SR as f32 / FFT_LEN as f32;
        if !(BAND_LO..BAND_HI).contains(&f) {
            continue;
        }
        let b = ((f / BAND_LO).ln() / step) as usize;
        bands[b.min(N_BANDS - 1)] += c.norm_sqr();
    }
    let total: f32 = bands.iter().sum();
    let hf: f32 = buf[1..FFT_LEN / 2]
        .iter()
        .enumerate()
        .filter(|(k, _)| ((k + 1) * SR / FFT_LEN) as f32 >= HF_SPLIT)
        .map(|(_, c)| c.norm_sqr())
        .sum();
    let hf_ratio = (hf / total).min(1.0);
    let mut spectrum: Vec<f32> = bands.iter().map(|e| e.log10()).collect();
    centre(&mut spectrum);

    // Envelope: 2 ms RMS frames from the onset, log, normalised to peak.
    let mut envelope: Vec<f32> = (0..ENV_FRAMES)
        .map(|i| {
            let s = onset + i * ENV_HOP;
            let r = audio[s..s + ENV_HOP].iter().map(|v| v * v).sum::<f32>() / ENV_HOP as f32;
            (r.sqrt() + 1e-6).log10()
        })
        .collect();
    let max = envelope.iter().cloned().fold(f32::MIN, f32::max);
    envelope.iter_mut().for_each(|v| *v -= max);

    Some(Signature { spectrum, envelope, peak: peak_at(audio, onset), hf_ratio })
}

fn centre(v: &mut [f32]) {
    let m = v.iter().sum::<f32>() / v.len() as f32;
    v.iter_mut().for_each(|x| *x -= m);
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

/// Weighted blend of spectral-shape correlation and decay-envelope match.
pub fn similarity(a: &Signature, b: &Signature) -> f32 {
    let spec = cosine(&a.spectrum, &b.spectrum);
    let mut ea = a.envelope.clone();
    let mut eb = b.envelope.clone();
    centre(&mut ea);
    centre(&mut eb);
    let env = cosine(&ea, &eb);
    (0.65 * spec + 0.35 * env).max(0.0)
}

/// Element-wise mean of several signatures (a cluster's template).
pub fn mean_signature(sigs: &[&Signature]) -> Signature {
    let n = sigs.len().max(1) as f32;
    let mut spectrum = vec![0f32; N_BANDS];
    let mut envelope = vec![0f32; ENV_FRAMES];
    for s in sigs {
        spectrum.iter_mut().zip(&s.spectrum).for_each(|(a, b)| *a += b / n);
        envelope.iter_mut().zip(&s.envelope).for_each(|(a, b)| *a += b / n);
    }
    centre(&mut spectrum);
    let mut peaks: Vec<f32> = sigs.iter().map(|s| s.peak).collect();
    let mut hfs: Vec<f32> = sigs.iter().map(|s| s.hf_ratio).collect();
    Signature { spectrum, envelope, peak: median(&mut peaks), hf_ratio: median(&mut hfs) }
}

pub fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// The `q` quantile (0..1) of `v`.
pub fn quantile(v: &mut [f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize]
}
