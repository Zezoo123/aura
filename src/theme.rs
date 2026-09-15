//! Color extraction from album art and the theme derived from it.

use image::DynamicImage;
use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub f32, pub f32, pub f32);

impl Rgb {
    pub fn from_u8(r: u8, g: u8, b: u8) -> Self {
        Rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }
    pub fn color(self) -> Color {
        Color::Rgb(
            (self.0.clamp(0.0, 1.0) * 255.0).round() as u8,
            (self.1.clamp(0.0, 1.0) * 255.0).round() as u8,
            (self.2.clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    }
    pub fn luma(self) -> f32 {
        0.2126 * self.0 + 0.7152 * self.1 + 0.0722 * self.2
    }
    pub fn saturation(self) -> f32 {
        let max = self.0.max(self.1).max(self.2);
        let min = self.0.min(self.1).min(self.2);
        if max <= 0.0001 {
            0.0
        } else {
            (max - min) / max
        }
    }
    pub fn hue(self) -> f32 {
        let max = self.0.max(self.1).max(self.2);
        let min = self.0.min(self.1).min(self.2);
        let d = max - min;
        if d < 1e-4 {
            return 0.0;
        }
        let h = if max == self.0 {
            ((self.1 - self.2) / d) % 6.0
        } else if max == self.1 {
            (self.2 - self.0) / d + 2.0
        } else {
            (self.0 - self.1) / d + 4.0
        };
        let h = h * 60.0;
        if h < 0.0 {
            h + 360.0
        } else {
            h
        }
    }
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        Rgb(
            self.0 + (other.0 - self.0) * t,
            self.1 + (other.1 - self.1) * t,
            self.2 + (other.2 - self.2) * t,
        )
    }
    pub fn scale(self, k: f32) -> Rgb {
        Rgb(self.0 * k, self.1 * k, self.2 * k)
    }
    /// Push saturation towards `target` (0..1) keeping hue and roughly the max channel.
    pub fn with_saturation(self, target: f32) -> Rgb {
        let max = self.0.max(self.1).max(self.2).max(1e-4);
        let gray = Rgb(max, max, max);
        let s = self.saturation();
        if s < 1e-4 {
            return self;
        }
        // Blend between gray (sat 0) and fully-saturated version (sat 1).
        let full = gray.lerp(self, 1.0 / s);
        gray.lerp(full, target.clamp(0.0, 1.0))
    }
    pub fn with_luma(self, target: f32) -> Rgb {
        let l = self.luma();
        if l < 1e-4 {
            return Rgb(target, target, target);
        }
        let k = target / l;
        let scaled = self.scale(k);
        // If scaling clips, blend towards white to hit the luma.
        let m = scaled.0.max(scaled.1).max(scaled.2);
        if m > 1.0 {
            let clipped = scaled.scale(1.0 / m);
            let need = (target - clipped.luma()) / (1.0 - clipped.luma()).max(1e-4);
            clipped.lerp(Rgb(1.0, 1.0, 1.0), need.clamp(0.0, 1.0))
        } else {
            scaled
        }
    }
    fn dist2(self, o: Rgb) -> f32 {
        (self.0 - o.0).powi(2) + (self.1 - o.1).powi(2) + (self.2 - o.2).powi(2)
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    /// Primary vibrant color from the art.
    pub accent: Rgb,
    /// Secondary color, hue-distinct from accent, used for gradients.
    pub accent2: Rgb,
    /// Readable body text color (tinted white).
    pub text: Rgb,
    /// Secondary text.
    pub muted: Rgb,
    /// Very dim text / inactive controls.
    pub dim: Rgb,
    /// Deep background tone derived from the art.
    pub bg: Rgb,
    /// Unfilled progress-track tone.
    pub track: Rgb,
    /// Raw dominant palette, most common first.
    pub palette: Vec<Rgb>,
}

impl Theme {
    pub fn fallback() -> Theme {
        let accent = Rgb::from_u8(30, 215, 96);
        let accent2 = Rgb::from_u8(130, 240, 190);
        Theme::from_accents(accent, accent2, Rgb::from_u8(12, 14, 16), vec![accent, accent2])
    }

    fn from_accents(accent: Rgb, accent2: Rgb, bg: Rgb, palette: Vec<Rgb>) -> Theme {
        let text = accent.with_saturation(0.10).with_luma(0.92);
        let muted = accent.with_saturation(0.25).with_luma(0.62);
        let dim = accent.with_saturation(0.30).with_luma(0.32);
        let track = accent.with_saturation(0.45).with_luma(0.10);
        Theme {
            accent,
            accent2,
            text,
            muted,
            dim,
            bg,
            track,
            palette,
        }
    }

    /// Derive a theme from album art with a small k-means clustering.
    pub fn from_image(img: &DynamicImage) -> Theme {
        let small = img.thumbnail(48, 48).to_rgb8();
        let pixels: Vec<Rgb> = small
            .pixels()
            .map(|p| Rgb::from_u8(p[0], p[1], p[2]))
            .collect();
        if pixels.is_empty() {
            return Theme::fallback();
        }
        let k = 6usize;
        // Seed with evenly spaced pixels, sorted by luma for spread.
        let mut sorted = pixels.clone();
        sorted.sort_by(|a, b| a.luma().partial_cmp(&b.luma()).unwrap());
        let mut centers: Vec<Rgb> = (0..k)
            .map(|i| sorted[(i * (sorted.len() - 1)) / (k - 1)])
            .collect();
        let mut counts = vec![0usize; k];
        for _ in 0..10 {
            let mut sums = vec![Rgb(0.0, 0.0, 0.0); k];
            counts.iter_mut().for_each(|c| *c = 0);
            for p in &pixels {
                let mut best = 0;
                let mut bd = f32::MAX;
                for (i, c) in centers.iter().enumerate() {
                    let d = p.dist2(*c);
                    if d < bd {
                        bd = d;
                        best = i;
                    }
                }
                sums[best].0 += p.0;
                sums[best].1 += p.1;
                sums[best].2 += p.2;
                counts[best] += 1;
            }
            for i in 0..k {
                if counts[i] > 0 {
                    centers[i] = sums[i].scale(1.0 / counts[i] as f32);
                }
            }
        }
        let mut clusters: Vec<(Rgb, usize)> = centers.into_iter().zip(counts).filter(|(_, n)| *n > 0).collect();
        clusters.sort_by(|a, b| b.1.cmp(&a.1));
        let total = pixels.len() as f32;
        let palette: Vec<Rgb> = clusters.iter().map(|(c, _)| *c).collect();

        // Score for accent: vibrant, mid-luma, and reasonably present.
        let score = |c: Rgb, n: usize| {
            let share = n as f32 / total;
            let sat = c.saturation();
            let l = c.luma();
            let luma_fit = 1.0 - ((l - 0.5).abs() * 1.6).min(1.0);
            sat * 1.6 + luma_fit * 0.9 + share.sqrt() * 0.5
        };
        let mut ranked: Vec<(Rgb, f32)> = clusters.iter().map(|(c, n)| (*c, score(*c, *n))).collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let raw_accent = ranked[0].0;
        // Guarantee legibility: min saturation and luma for the accent.
        let mut accent = raw_accent;
        if accent.saturation() < 0.35 {
            accent = accent.with_saturation(0.45);
        }
        if accent.luma() < 0.42 {
            accent = accent.with_luma(0.55);
        } else if accent.luma() > 0.85 {
            accent = accent.with_luma(0.78);
        }

        // Secondary: the cluster whose hue differs most from the accent, among vibrant ones.
        let accent2 = ranked
            .iter()
            .skip(1)
            .filter(|(c, _)| c.saturation() > 0.18)
            .max_by(|(a, _), (b, _)| {
                let da = hue_dist(a.hue(), accent.hue());
                let db = hue_dist(b.hue(), accent.hue());
                da.partial_cmp(&db).unwrap()
            })
            .map(|(c, _)| *c)
            .map(|c| {
                let mut c = c;
                if c.luma() < 0.45 {
                    c = c.with_luma(0.6);
                }
                if c.saturation() < 0.3 {
                    c = c.with_saturation(0.4);
                }
                c
            })
            .unwrap_or_else(|| accent.with_luma((accent.luma() + 0.25).min(0.9)));

        // Background: darkest cluster, pulled down to a deep tone but keeping its hue.
        let darkest = clusters
            .iter()
            .map(|(c, _)| *c)
            .min_by(|a, b| a.luma().partial_cmp(&b.luma()).unwrap())
            .unwrap_or(Rgb(0.05, 0.05, 0.06));
        let bg = darkest.with_saturation(darkest.saturation().max(0.25)).with_luma(0.045);

        Theme::from_accents(accent, accent2, bg, palette)
    }

    /// Blend every color of this theme towards `other` by `t` (0..1).
    pub fn lerp(&self, other: &Theme, t: f32) -> Theme {
        Theme {
            accent: self.accent.lerp(other.accent, t),
            accent2: self.accent2.lerp(other.accent2, t),
            text: self.text.lerp(other.text, t),
            muted: self.muted.lerp(other.muted, t),
            dim: self.dim.lerp(other.dim, t),
            bg: self.bg.lerp(other.bg, t),
            track: self.track.lerp(other.track, t),
            palette: if t < 0.5 { self.palette.clone() } else { other.palette.clone() },
        }
    }

    /// Color at position t (0..1) along the accent -> accent2 gradient.
    pub fn gradient(&self, t: f32) -> Rgb {
        self.accent.lerp(self.accent2, t.clamp(0.0, 1.0))
    }
}

fn hue_dist(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// A tiny blurred, darkened copy of the art used as an ambient backdrop.
pub struct Ambient {
    w: u32,
    h: u32,
    px: Vec<Rgb>,
}

impl Ambient {
    pub fn from_image(img: &DynamicImage, theme: &Theme) -> Ambient {
        let small = img.thumbnail(40, 40).fast_blur(3.0).to_rgb8();
        let (w, h) = small.dimensions();
        let px = small
            .pixels()
            .map(|p| {
                let c = Rgb::from_u8(p[0], p[1], p[2]);
                // Keep the hue, crush the brightness so text stays readable.
                let target = 0.035 + c.luma() * 0.075;
                c.with_saturation((c.saturation() * 1.2).min(0.8))
                    .with_luma(target)
                    .lerp(theme.bg, 0.25)
            })
            .collect();
        Ambient { w, h, px }
    }

    /// Sample the backdrop for a terminal cell at normalized coordinates.
    pub fn sample(&self, u: f32, v: f32) -> Rgb {
        let x = ((u.clamp(0.0, 0.9999)) * self.w as f32) as u32;
        let y = ((v.clamp(0.0, 0.9999)) * self.h as f32) as u32;
        let c = self.px[(y * self.w + x) as usize];
        // Soft vignette towards the edges.
        let dx = u - 0.5;
        let dy = v - 0.5;
        let r = (dx * dx + dy * dy).sqrt();
        let k = 1.0 - (r * 0.9).min(0.55);
        c.scale(k)
    }
}
