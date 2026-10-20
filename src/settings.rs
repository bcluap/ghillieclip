//! Every user-tunable setting, shared by the CLI, the GUI and saved JSON files.

use crate::bb::TrackParams;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Video files and/or folders of videos to process.
    pub inputs: Vec<PathBuf>,
    /// Highlight reel to write.
    pub output: PathBuf,
    pub profile: ProfileSource,
    pub detect: DetectParams,
    pub clips: ClipParams,
    pub track: TrackParams,
    pub markers: MarkerParams,
    pub distance: DistanceParams,
    pub ballistics: Ballistics,
    pub encode: EncodeParams,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            inputs: Vec::new(),
            output: PathBuf::from("shots_highlight.mp4"),
            profile: ProfileSource::default(),
            detect: DetectParams::default(),
            clips: ClipParams::default(),
            track: TrackParams::default(),
            markers: MarkerParams::default(),
            distance: DistanceParams::default(),
            ballistics: Ballistics::default(),
            encode: EncodeParams::default(),
        }
    }
}

/// Where the shot/BB profile comes from.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ProfileSource {
    /// Learn from the input footage before processing.
    Learn,
    /// The profile built into the program (SC024K HPA sniper rifle).
    BuiltIn,
    /// A profile saved earlier.
    File(PathBuf),
}

impl Default for ProfileSource {
    fn default() -> Self {
        ProfileSource::Learn
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DetectParams {
    /// Minimum similarity (0..1) to the profile's shot sound.
    pub threshold: f32,
    /// Minimum loudness relative to the profile's typical shot (filters other guns).
    pub min_rel_level: f32,
    /// Maximum share of energy above 4 kHz (rejects bolt/cocking clicks).
    pub max_hf: f32,
    /// Shots closer together than this (s) count as one (echoes).
    pub min_shot_gap: f32,
    /// Energy jump over background that marks a sound's onset.
    pub onset_ratio: f32,
    /// A shot sound only counts if a BB is found and still seen at least
    /// this long (s) after the shot. 0 = count every matching sound.
    pub require_bb_secs: f32,
}

impl Default for DetectParams {
    fn default() -> Self {
        DetectParams {
            threshold: 0.85,
            min_rel_level: 0.45,
            max_hf: 0.30,
            min_shot_gap: 0.25,
            onset_ratio: 8.0,
            require_bb_secs: 0.2,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClipParams {
    /// Seconds of footage kept before each shot.
    pub pre_secs: f32,
    /// Seconds of footage kept after each shot.
    pub post_secs: f32,
}

impl Default for ClipParams {
    fn default() -> Self {
        ClipParams { pre_secs: 5.0, post_secs: 5.0 }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MarkerStyle {
    BloodSplatter,
    Crosshair,
    Explosion,
}

impl MarkerStyle {
    pub const ALL: [MarkerStyle; 3] = [MarkerStyle::BloodSplatter, MarkerStyle::Crosshair, MarkerStyle::Explosion];
    pub fn label(self) -> &'static str {
        match self {
            MarkerStyle::BloodSplatter => "Blood splatter",
            MarkerStyle::Crosshair => "Crosshair",
            MarkerStyle::Explosion => "Explosion",
        }
    }
}

impl std::str::FromStr for MarkerStyle {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "blood" | "bloodsplatter" | "splatter" => Ok(MarkerStyle::BloodSplatter),
            "crosshair" | "cross" => Ok(MarkerStyle::Crosshair),
            "explosion" | "boom" => Ok(MarkerStyle::Explosion),
            _ => Err(format!("unknown marker style `{s}` (blood, crosshair, explosion)")),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MarkerParams {
    /// Draw a marker where a BB impact is detected.
    pub enabled: bool,
    pub style: MarkerStyle,
    /// Main colour (crosshair colour, blood colour; explosion uses a fire palette tinted by it).
    pub colour: [u8; 3],
    /// Marker diameter, px.
    pub size_px: f32,
    /// How long the marker stays on screen, s.
    pub duration_secs: f32,
}

impl Default for MarkerParams {
    fn default() -> Self {
        MarkerParams {
            enabled: false,
            style: MarkerStyle::Crosshair,
            colour: [255, 40, 40],
            size_px: 80.0,
            duration_secs: 0.8,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Units {
    Metres,
    Feet,
}

impl Units {
    pub fn label(self) -> &'static str {
        match self {
            Units::Metres => "m",
            Units::Feet => "ft",
        }
    }
}

impl std::str::FromStr for Units {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "m" | "metres" | "meters" | "metre" | "meter" => Ok(Units::Metres),
            "ft" | "feet" | "foot" => Ok(Units::Feet),
            _ => Err(format!("unknown units `{s}` (m, ft)")),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DistanceParams {
    /// Show the impact distance above the impact point.
    pub at_impact: bool,
    /// Show the BB's current distance next to it on every tracked frame.
    pub live: bool,
    pub units: Units,
    /// Text height, px.
    pub font_px: f32,
    pub colour: [u8; 3],
    /// Decimal places.
    pub decimals: u8,
}

impl Default for DistanceParams {
    fn default() -> Self {
        DistanceParams { at_impact: false, live: false, units: Units::Metres, font_px: 48.0, colour: [255, 255, 255], decimals: 1 }
    }
}

/// BB flight model: a sphere slowed by aerodynamic drag,
/// dv/dt = -k v², k = ½ ρ Cd A / m, so v(t) = v0 / (1 + k v0 t) and
/// distance(t) = ln(1 + k v0 t) / k. Hop-up lift and gravity are ignored
/// (they barely change the distance travelled along the flight path).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ballistics {
    pub bb_mass_g: f32,
    pub muzzle_fps: f32,
    pub bb_diameter_mm: f32,
    pub drag_coefficient: f32,
    /// kg/m³ (1.225 at sea level, 15 °C).
    pub air_density: f32,
}

impl Default for Ballistics {
    fn default() -> Self {
        Ballistics { bb_mass_g: 0.40, muzzle_fps: 450.0, bb_diameter_mm: 5.95, drag_coefficient: 0.47, air_density: 1.225 }
    }
}

impl Ballistics {
    fn k_v0(&self) -> (f64, f64) {
        let m = (self.bb_mass_g as f64 / 1000.0).max(1e-6);
        let r = self.bb_diameter_mm as f64 / 2000.0;
        let area = std::f64::consts::PI * r * r;
        let k = 0.5 * self.air_density as f64 * self.drag_coefficient as f64 * area / m;
        (k, self.muzzle_fps as f64 * 0.3048)
    }

    /// Distance travelled (m) `t` seconds after leaving the muzzle.
    pub fn distance_m(&self, t: f64) -> f64 {
        let (k, v0) = self.k_v0();
        if t <= 0.0 {
            0.0
        } else if k <= 0.0 {
            v0 * t
        } else {
            (1.0 + k * v0 * t).ln() / k
        }
    }

    /// Speed (m/s) `t` seconds after leaving the muzzle.
    pub fn speed_ms(&self, t: f64) -> f64 {
        let (k, v0) = self.k_v0();
        v0 / (1.0 + k * v0 * t.max(0.0))
    }

    /// Format a distance in metres in the chosen units.
    pub fn format(dist_m: f64, d: &DistanceParams) -> String {
        let v = match d.units {
            Units::Metres => dist_m,
            Units::Feet => dist_m / 0.3048,
        };
        format!("{v:.*} {}", d.decimals as usize, d.units.label())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EncodeParams {
    /// Stream-copy cuts (fast, keyframe-snapped; no highlights or markers).
    pub copy: bool,
    /// x264 quality (lower = better, bigger).
    pub crf: u8,
    /// x264 speed preset.
    pub preset: String,
    /// Shots tracked in parallel.
    pub threads: usize,
}

impl Default for EncodeParams {
    fn default() -> Self {
        EncodeParams { copy: false, crf: 18, preset: "veryfast".into(), threads: 3 }
    }
}

impl EncodeParams {
    /// x264/AAC settings shared by every re-encoded segment so they concat cleanly.
    pub fn args(&self) -> Vec<String> {
        [
            "-c:v", "libx264", "-preset", &self.preset, "-crf", &self.crf.to_string(), "-pix_fmt", "yuv420p",
            "-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709",
            "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2",
        ]
        .map(String::from)
        .to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_model_slows_bb() {
        let b = Ballistics::default();
        let v0 = b.muzzle_fps as f64 * 0.3048;
        assert!((b.speed_ms(0.0) - v0).abs() < 1e-9);
        assert!(b.speed_ms(0.5) < v0);
        // Distance never exceeds drag-free flight and grows with time.
        assert!(b.distance_m(0.5) < v0 * 0.5);
        assert!(b.distance_m(0.6) > b.distance_m(0.5));
    }
}
