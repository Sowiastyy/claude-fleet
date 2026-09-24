//! The pane's picture while the fleet runs nothing: its name in block letters,
//! given depth and turning about the vertical axis.
//!
//! Every cell casts a ray from a camera in front of the pane. The letters are
//! boxes, one for each run of lit pixels in a row of the font, so what a ray
//! meets is a face of one of them; how squarely that face turns to a light
//! picks the character, the way the spinning-donut demos shade theirs.

use std::{
    f32::consts::TAU,
    sync::OnceLock,
    time::{Duration, Instant},
};

/// How often the picture wants a new frame.
pub const FRAME: Duration = Duration::from_millis(40);
/// Seconds for one full turn.
const PERIOD: f32 = 8.0;
/// Darkest to brightest.
const RAMP: &[u8] = b".,-~:;=!*#$@";
/// A terminal cell is about twice as tall as it is wide.
const CELL_ASPECT: f32 = 2.0;
/// Half the letters' depth, in pixels of the font.
const DEPTH: f32 = 1.2;
/// The camera stands this many times the letters' reach away. Closer
/// exaggerates the perspective, further flattens it.
const CAMERA: f32 = 4.0;
/// Below this many rows to a pixel of the font, rows of it start falling
/// between cells and the letters stop reading as letters.
const MIN_ROWS: f32 = 1.0;
/// The same across. A narrow pane squeezes the letters rather than losing
/// them, but only this far.
const MIN_COLS: f32 = 1.2;
/// Above it the letters only get coarser.
const MAX_ROWS: f32 = 2.5;
/// Where the light comes from: above, to the left, in front.
const LIGHT: [f32; 3] = [-0.45, 0.55, 0.7];
/// Rows between the two words when they are stacked.
const LINE_GAP: usize = 2;
const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;

#[rustfmt::skip]
fn glyph(c: char) -> [&'static str; GLYPH_H] {
    match c {
        'A' => [".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
        'C' => [".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."],
        'D' => ["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."],
        'E' => ["#####", "#....", "#....", "####.", "#....", "#....", "#####"],
        'F' => ["#####", "#....", "#....", "####.", "#....", "#....", "#...."],
        'L' => ["#....", "#....", "#....", "#....", "#....", "#....", "#####"],
        'T' => ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."],
        'U' => ["#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
        _ => ["....."; GLYPH_H],
    }
}

/// The name set in one of the ways it can stand on the pane.
struct Letters {
    /// Runs of lit pixels as `(left, right)`, by row of the font from the top.
    rows: Vec<Vec<(f32, f32)>>,
    half_w: f32,
    half_h: f32,
}

impl Letters {
    fn new(lines: &[&str]) -> Self {
        let height = lines.len() * GLYPH_H + (lines.len() - 1) * LINE_GAP;
        let mut rows = vec![Vec::new(); height];
        let mut widest = 0;
        for (n, line) in lines.iter().enumerate() {
            let glyphs: Vec<_> = line.chars().map(glyph).collect();
            let width = glyphs.len() * (GLYPH_W + 1) - 1;
            widest = widest.max(width);
            let left = -(width as f32) / 2.0;
            for gy in 0..GLYPH_H {
                let bits: Vec<bool> = glyphs
                    .iter()
                    .enumerate()
                    .flat_map(|(i, g)| {
                        // A column of space between letters.
                        let gap = (i > 0).then_some(false);
                        gap.into_iter().chain(g[gy].bytes().map(|b| b == b'#'))
                    })
                    .collect();
                let row = &mut rows[n * (GLYPH_H + LINE_GAP) + gy];
                let mut x = 0;
                while x < bits.len() {
                    if !bits[x] {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < bits.len() && bits[x] {
                        x += 1;
                    }
                    row.push((left + start as f32, left + x as f32));
                }
            }
        }
        Letters {
            rows,
            half_w: widest as f32 / 2.0,
            half_h: height as f32 / 2.0,
        }
    }

    /// The furthest the letters reach from the axis they turn about.
    fn reach(&self) -> f32 {
        self.half_w.hypot(DEPTH)
    }

    /// Columns and rows of the pane to a pixel of the font: the largest that
    /// keep the letters inside `width` by `height` however they are turned.
    fn scale(&self, width: u16, height: u16) -> Option<(f32, f32)> {
        // What turns towards the camera comes out this much larger.
        let near = CAMERA / (CAMERA - 1.0);
        // A cell spare on every side, so nothing is ever clipped at the frame.
        let across = (width as f32 / 2.0 - 1.0) / (self.reach() * near);
        let down = (height as f32 / 2.0 - 1.0) / (self.half_h * near);
        // Square pixels where there is room, squeezed sideways where there is
        // not — but never to fewer columns than rows, or they read as bars.
        let rows = down.min(across).min(MAX_ROWS);
        let cols = across.min(rows * CELL_ASPECT);
        (cols >= MIN_COLS && rows >= MIN_ROWS).then_some((cols, rows))
    }

    /// The face a ray meets first, as its axis and which way it points, all in
    /// the letters' own frame.
    fn trace(&self, origin: [f32; 3], dir: [f32; 3]) -> Option<(usize, f32)> {
        // A ray running exactly along a face would divide zero by zero.
        let inv = dir.map(|c| 1.0 / if c.abs() < 1e-6 { 1e-6 } else { c });
        let (enter, leave, _) = slab(
            origin,
            inv,
            [-self.half_w, -self.half_h, -DEPTH],
            [self.half_w, self.half_h, DEPTH],
        )?;
        // Only the rows of the font the ray crosses while inside the bounds.
        let (ya, yb) = (origin[1] + enter * dir[1], origin[1] + leave * dir[1]);
        let first = (self.half_h - ya.max(yb)).floor().max(0.0) as usize;
        let last = ((self.half_h - ya.min(yb)).floor().max(0.0) as usize).min(self.rows.len() - 1);
        let mut best: Option<(f32, usize)> = None;
        for j in first..=last {
            let top = self.half_h - j as f32;
            for &(x0, x1) in &self.rows[j] {
                if let Some((t, _, axis)) =
                    slab(origin, inv, [x0, top - 1.0, -DEPTH], [x1, top, DEPTH])
                    && best.is_none_or(|(b, _)| t < b)
                {
                    best = Some((t, axis));
                }
            }
        }
        best.map(|(_, axis)| (axis, -dir[axis].signum()))
    }
}

/// Where a ray enters and leaves a box, and the axis of the face it enters by.
fn slab(origin: [f32; 3], inv: [f32; 3], lo: [f32; 3], hi: [f32; 3]) -> Option<(f32, f32, usize)> {
    let mut enter = f32::NEG_INFINITY;
    let mut leave = f32::INFINITY;
    let mut axis = 0;
    for k in 0..3 {
        let a = (lo[k] - origin[k]) * inv[k];
        let b = (hi[k] - origin[k]) * inv[k];
        let (near, far) = if a < b { (a, b) } else { (b, a) };
        if near > enter {
            enter = near;
            axis = k;
        }
        leave = leave.min(far);
    }
    (enter <= leave && enter > 0.0).then_some((enter, leave, axis))
}

fn layouts() -> &'static [Letters; 2] {
    static LAYOUTS: OnceLock<[Letters; 2]> = OnceLock::new();
    LAYOUTS.get_or_init(|| {
        [
            Letters::new(&["CLAUDE", "FLEET"]),
            Letters::new(&["CLAUDE FLEET"]),
        ]
    })
}

/// How far the letters have turned by now. They face the camera the moment
/// the fleet starts.
pub fn angle() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    (t / PERIOD).fract() * TAU
}

/// One frame, `width` by `height` cells row by row, with the letters turned
/// `angle` radians: the character for each cell the letters cover and how lit
/// it is, from 0 to 1. `None` when the area is too small to show them at all.
pub fn render(width: u16, height: u16, angle: f32) -> Option<Vec<Option<(char, f32)>>> {
    // Two words stacked suit most panes; one line suits a wide, short one.
    // Whichever shows the letters larger and less squeezed wins.
    let (letters, (sx, sy)) = layouts()
        .iter()
        .filter_map(|l| Some((l, l.scale(width, height)?)))
        .max_by(|(_, a), (_, b)| {
            let size = |(cols, rows): (f32, f32)| rows.min(cols / CELL_ASPECT);
            size(*a).total_cmp(&size(*b))
        })?;
    let dist = CAMERA * letters.reach();
    let (sin, cos) = angle.sin_cos();
    let len = LIGHT.iter().map(|c| c * c).sum::<f32>().sqrt();
    let light = LIGHT.map(|c| c / len);

    // The camera looks down -z from `dist` in front; the letters turning by
    // `angle` is the camera turning by `-angle` about them.
    let origin = [-dist * sin, 0.0, dist * cos];
    let mut cells = Vec::with_capacity(width as usize * height as usize);
    for row in 0..height {
        let v = (height as f32 / 2.0 - row as f32 - 0.5) / sy;
        for col in 0..width {
            let u = (col as f32 + 0.5 - width as f32 / 2.0) / sx;
            let dir = [u * cos + dist * sin, v, u * sin - dist * cos];
            cells.push(letters.trace(origin, dir).map(|(axis, sign)| {
                let mut n = [0.0; 3];
                n[axis] = sign;
                let n = [n[0] * cos + n[2] * sin, n[1], n[2] * cos - n[0] * sin];
                let lit = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.0);
                let b = 0.15 + 0.85 * lit;
                let i = (b * (RAMP.len() - 1) as f32).round() as usize;
                (RAMP[i.min(RAMP.len() - 1)] as char, b)
            }));
        }
    }
    Some(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(cells: &[Option<(char, f32)>]) -> usize {
        cells.iter().filter(|c| c.is_some()).count()
    }

    /// Columns holding at least one lit cell.
    fn span(cells: &[Option<(char, f32)>], width: u16) -> (usize, usize) {
        let cols: Vec<usize> = cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_some())
            .map(|(i, _)| i % width as usize)
            .collect();
        (*cols.iter().min().unwrap(), *cols.iter().max().unwrap())
    }

    #[test]
    fn the_letters_stay_inside_the_pane_all_the_way_round() {
        for (w, h) in [(80, 24), (120, 40), (200, 18), (60, 60)] {
            for step in 0..64 {
                let angle = step as f32 / 64.0 * TAU;
                let cells = render(w, h, angle).expect("fits");
                let top_row = &cells[..w as usize];
                let bottom_row = &cells[cells.len() - w as usize..];
                assert!(top_row.iter().all(Option::is_none), "{w}x{h} at {angle}");
                assert!(bottom_row.iter().all(Option::is_none), "{w}x{h} at {angle}");
                let (left, right) = span(&cells, w);
                assert!(left > 0 && right < w as usize - 1, "{w}x{h} at {angle}");
            }
        }
    }

    #[test]
    fn edge_on_the_letters_cover_far_less_than_face_on() {
        let face = render(120, 40, 0.0).unwrap();
        let edge = render(120, 40, TAU / 4.0).unwrap();
        let back = render(120, 40, TAU / 2.0).unwrap();
        // Perspective still shows the sides of the nearer letters.
        assert!(
            lit(&edge) * 2 < lit(&face),
            "{} vs {}",
            lit(&edge),
            lit(&face)
        );
        // From behind it is the same letters, mirrored.
        let (f, b) = (lit(&face) as f32, lit(&back) as f32);
        assert!((f - b).abs() / f < 0.1, "{f} vs {b}");
    }

    #[test]
    fn face_on_every_row_of_the_font_lands_on_a_row_of_the_pane() {
        // The E's middle bar is a single row of the font; at the smallest scale
        // it still has to show up.
        let (w, h) = (90, 26);
        let cells = render(w, h, 0.0).expect("fits");
        let rows_lit = (0..h as usize)
            .filter(|r| {
                cells[r * w as usize..(r + 1) * w as usize]
                    .iter()
                    .any(Option::is_some)
            })
            .count();
        assert!(rows_lit >= 2 * GLYPH_H, "{rows_lit}");
    }

    #[test]
    fn a_pane_too_small_for_the_letters_gets_none() {
        assert!(render(30, 8, 0.0).is_none());
        assert!(render(0, 0, 0.0).is_none());
    }

    #[test]
    fn a_wide_short_pane_sets_the_name_on_one_line() {
        let (w, h) = (200, 14);
        let cells = render(w, h, 0.0).expect("fits");
        let rows_lit = (0..h as usize)
            .filter(|r| {
                cells[r * w as usize..(r + 1) * w as usize]
                    .iter()
                    .any(Option::is_some)
            })
            .count();
        assert!(rows_lit < 2 * GLYPH_H, "{rows_lit}");
    }
}
