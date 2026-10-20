//! A gun/camera profile: what a shot sounds like and where its BB appears.
//! Learned from footage (see `learn`), built in, or loaded from JSON.

use crate::audio::Signature;
use crate::settings::{DetectParams, Settings};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    /// Human-readable description (where it was learned from).
    pub name: String,
    /// Typical shot sound.
    pub signature: Signature,
    /// Detection thresholds suited to this gun.
    pub detect: DetectParams,
    /// Where/which way the BB first appears in the camera view, if known.
    pub launch: Option<Launch>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Launch {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub dir_deg: f32,
    pub dir_tol_deg: f32,
}

impl Profile {
    pub fn load(path: &Path) -> Result<Profile> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing profile {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    /// Copy this profile's thresholds and launch geometry into `s`.
    pub fn apply(&self, s: &mut Settings) {
        s.detect.threshold = self.detect.threshold;
        s.detect.min_rel_level = self.detect.min_rel_level;
        s.detect.max_hf = self.detect.max_hf;
        if let Some(l) = self.launch {
            s.track.launch_x = l.x;
            s.track.launch_y = l.y;
            s.track.launch_radius = l.radius;
            s.track.launch_dir_deg = l.dir_deg;
            s.track.launch_dir_tol_deg = l.dir_tol_deg;
        }
    }

    pub fn summary(&self) -> String {
        let launch = self.launch.map_or("launch point: default".into(), |l| {
            format!(
                "launch point ({:.3}, {:.3}) ±{:.3}, direction {:.0}°±{:.0}°",
                l.x, l.y, l.radius, l.dir_deg, l.dir_tol_deg
            )
        });
        format!(
            "{}\nshot level {:.3}, similarity ≥ {:.2}, level ≥ {:.2}, HF ≤ {:.2}; {launch}",
            self.name, self.signature.peak, self.detect.threshold, self.detect.min_rel_level, self.detect.max_hf
        )
    }
}
