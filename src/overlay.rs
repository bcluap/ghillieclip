//! Drawing onto yuv420p frames: BB highlight discs, animated impact markers
//! and outlined text. Colours are sRGB, converted to limited-range BT.709.

use crate::settings::MarkerStyle;
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use std::sync::OnceLock;

static FONT_DATA: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(FONT_DATA).expect("bundled font is valid"))
}

/// sRGB to limited-range BT.709 (Y, Cb, Cr).
pub fn rgb_to_yuv([r, g, b]: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    [16.0 + 219.0 * y, 128.0 + 224.0 * (b - y) / 1.8556, 128.0 + 224.0 * (r - y) / 1.5748]
}

/// A yuv420p frame being drawn on.
pub struct Canvas<'a> {
    buf: &'a mut [u8],
    pub w: usize,
    pub h: usize,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u8], w: usize, h: usize) -> Self {
        Canvas { buf, w, h }
    }

    /// Alpha-blend one pixel. Chroma (half resolution) is blended from the
    /// top-left pixel of each 2x2 block.
    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, yuv: [f32; 3], alpha: f32) {
        if alpha <= 0.0 || x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let a = alpha.min(1.0);
        let (x, y) = (x as usize, y as usize);
        let p = &mut self.buf[y * self.w + x];
        *p = (*p as f32 * (1.0 - a) + yuv[0] * a).round() as u8;
        if x % 2 == 0 && y % 2 == 0 {
            let (cw, ch) = (self.w / 2, self.h / 2);
            let (cx, cy) = (x / 2, y / 2);
            if cx < cw && cy < ch {
                let u = self.w * self.h + cy * cw + cx;
                let v = u + cw * ch;
                self.buf[u] = (self.buf[u] as f32 * (1.0 - a) + yuv[1] * a).round() as u8;
                self.buf[v] = (self.buf[v] as f32 * (1.0 - a) + yuv[2] * a).round() as u8;
            }
        }
    }

    /// Anti-aliased filled disc.
    pub fn disc(&mut self, cx: f32, cy: f32, r: f32, rgb: [u8; 3], alpha: f32) {
        let yuv = rgb_to_yuv(rgb);
        let reach = r + 1.0;
        for y in (cy - reach).floor() as i32..=(cy + reach).ceil() as i32 {
            for x in (cx - reach).floor() as i32..=(cx + reach).ceil() as i32 {
                let d = (x as f32 - cx).hypot(y as f32 - cy);
                self.blend(x, y, yuv, (r + 0.5 - d).clamp(0.0, 1.0) * alpha);
            }
        }
    }

    /// Anti-aliased ring of thickness `t`.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, t: f32, rgb: [u8; 3], alpha: f32) {
        let yuv = rgb_to_yuv(rgb);
        let reach = r + t + 1.0;
        for y in (cy - reach).floor() as i32..=(cy + reach).ceil() as i32 {
            for x in (cx - reach).floor() as i32..=(cx + reach).ceil() as i32 {
                let d = ((x as f32 - cx).hypot(y as f32 - cy) - r).abs();
                self.blend(x, y, yuv, (t / 2.0 + 0.5 - d).clamp(0.0, 1.0) * alpha);
            }
        }
    }

    /// Anti-aliased line segment of thickness `t`.
    pub fn line(&mut self, (x0, y0): (f32, f32), (x1, y1): (f32, f32), t: f32, rgb: [u8; 3], alpha: f32) {
        let yuv = rgb_to_yuv(rgb);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let pad = t + 1.0;
        for y in (y0.min(y1) - pad).floor() as i32..=(y0.max(y1) + pad).ceil() as i32 {
            for x in (x0.min(x1) - pad).floor() as i32..=(x0.max(x1) + pad).ceil() as i32 {
                let (px, py) = (x as f32 - x0, y as f32 - y0);
                let u = ((px * dx + py * dy) / len2).clamp(0.0, 1.0);
                let d = (px - u * dx).hypot(py - u * dy);
                self.blend(x, y, yuv, (t / 2.0 + 0.5 - d).clamp(0.0, 1.0) * alpha);
            }
        }
    }

    /// Text with a dark outline. `anchor` is where (ax, ay) sits on the text
    /// box: (0..1 horizontally, 0..1 vertically), e.g. (0.5, 1.0) = bottom centre.
    pub fn text(&mut self, s: &str, ax: f32, ay: f32, anchor: (f32, f32), px: f32, rgb: [u8; 3], alpha: f32) {
        let f = font();
        let scale = PxScale::from(px);
        let sf = f.as_scaled(scale);
        let mut glyphs = Vec::new();
        let mut caret = 0.0f32;
        let mut prev = None;
        for ch in s.chars() {
            let id = f.glyph_id(ch);
            if let Some(p) = prev {
                caret += sf.kern(p, id);
            }
            glyphs.push(id.with_scale_and_position(scale, ab_glyph::point(caret, sf.ascent())));
            caret += sf.h_advance(id);
            prev = Some(id);
        }
        let (tw, th) = (caret, sf.ascent() - sf.descent());
        let ox = ax - anchor.0 * tw;
        let oy = ay - anchor.1 * th;
        let outline = (px / 14.0).max(1.5);
        let mut cov = Vec::new();
        for g in glyphs {
            if let Some(o) = f.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, c| cov.push((b.min.x + gx as f32, b.min.y + gy as f32, c)));
            }
        }
        // Outline: dilate coverage by drawing black copies around a circle.
        let dark = rgb_to_yuv([0, 0, 0]);
        let steps = 12;
        for i in 0..steps {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            let (dx, dy) = (a.cos() * outline, a.sin() * outline);
            for &(x, y, c) in &cov {
                self.blend((ox + x + dx).round() as i32, (oy + y + dy).round() as i32, dark, c * alpha * 0.85);
            }
        }
        let yuv = rgb_to_yuv(rgb);
        for &(x, y, c) in &cov {
            self.blend((ox + x).round() as i32, (oy + y).round() as i32, yuv, c * alpha);
        }
    }
}

/// Small deterministic RNG so a marker looks the same on every frame.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.next()
    }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn shade(c: [u8; 3], f: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * f).clamp(0.0, 255.0) as u8)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8)
}

/// Draw an impact marker at phase `t` (0 = impact, 1 = end of its display).
pub fn marker(c: &mut Canvas, style: MarkerStyle, cx: f32, cy: f32, size: f32, colour: [u8; 3], t: f32, seed: u64) {
    let r = size / 2.0;
    // Fade out over the last 30 %.
    let fade = if t > 0.7 { 1.0 - (t - 0.7) / 0.3 } else { 1.0 };
    match style {
        MarkerStyle::Crosshair => {
            // "Lock on": shrinks from 1.6x to 1x over the first 20 %.
            let s = r * (1.0 + 0.6 * (1.0 - ease_out(t / 0.2)));
            let th = (size / 22.0).max(2.0);
            c.ring(cx, cy, s, th, colour, fade);
            for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                c.line((cx + dx * s * 0.35, cy + dy * s * 0.35), (cx + dx * s * 1.35, cy + dy * s * 1.35), th, colour, fade);
            }
            c.disc(cx, cy, th * 0.8, colour, fade);
        }
        MarkerStyle::BloodSplatter => {
            let mut rng = Rng::new(seed);
            let grow = ease_out(t / 0.15);
            c.disc(cx, cy, r * 0.22 * grow, shade(colour, 0.75), fade);
            for _ in 0..16 {
                let ang = rng.range(0.0, std::f32::consts::TAU);
                let dist = rng.range(0.15, 0.55) * r * 2.0 * grow;
                let rad = rng.range(0.03, 0.10) * size;
                let col = shade(colour, rng.range(0.55, 1.0));
                c.disc(cx + ang.cos() * dist, cy + ang.sin() * dist, rad * grow.max(0.3), col, fade);
            }
            // Streaks: tapering chains of droplets.
            for _ in 0..5 {
                let ang = rng.range(0.0, std::f32::consts::TAU);
                let len = rng.range(0.5, 1.0) * r * 1.4 * grow;
                let col = shade(colour, rng.range(0.6, 0.9));
                for k in 0..8 {
                    let f = k as f32 / 8.0;
                    let rad = size * 0.06 * (1.0 - f) + 1.0;
                    c.disc(cx + ang.cos() * len * f, cy + ang.sin() * len * f, rad, col, fade);
                }
            }
        }
        MarkerStyle::Explosion => {
            let mut rng = Rng::new(seed);
            let core = [255, 255, 225];
            let mid = [255, 196, 64];
            let edge = mix([255, 90, 20], colour, 0.5);
            let a = (1.0 - t).powf(1.5);
            let fire = r * ease_out(t / 0.35);
            // Fireball: concentric discs from the edge colour in to the core.
            for i in 0..6 {
                let f = 1.0 - i as f32 / 6.0;
                let col = if f > 0.5 { mix(mid, edge, (f - 0.5) * 2.0) } else { mix(core, mid, f * 2.0) };
                c.disc(cx, cy, fire * f, col, a * 0.85);
            }
            // Shock ring and sparks.
            c.ring(cx, cy, r * (0.3 + 1.2 * ease_out(t)), (size / 30.0).max(2.0), mid, a * 0.7);
            for _ in 0..18 {
                let ang = rng.range(0.0, std::f32::consts::TAU);
                let speed = rng.range(0.8, 1.6);
                let d = r * speed * ease_out(t);
                c.disc(cx + ang.cos() * d, cy + ang.sin() * d, (size / 40.0).max(1.5), mid, a);
            }
        }
    }
}
