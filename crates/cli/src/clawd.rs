//! Clawd, Claude Code's mascot, dancing in a corner of the empty pane.
//!
//! It is drawn the way Claude Code draws it, in quadrant blocks: a cell of the
//! pane holds two by two pixels. Each pose of the dance is put together from
//! the body, where it stands, its arms and feet, and which way it looks. The
//! body only ever moves a whole cell at a time; half a cell off, the blocks no
//! longer line up with its eyes and it turns to mush.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

/// Pixels across: the body, an arm either side, and a step of sway beyond.
const W: usize = 22;
/// Pixels down: the body, its feet, and a step of air below them in a hop.
const H: usize = 7;
const BODY_W: i32 = 12;
const BODY_H: i32 = 4;
/// How far a sway or a hop moves the body: a cell of the pane.
const STEP: i32 = 2;
/// Where the body's left edge stands when it is not swaying. An odd pixel,
/// as in Claude Code, so its edges fall mid-cell.
const HOME: i32 = 5;
/// Eyes, from the body's left edge, on its second row.
const EYES: [i32; 2] = [2, 9];
/// Feet, from the body's left edge when at home: a pair under each side.
const FEET: [[i32; 2]; 2] = [[1, 3], [8, 10]];
/// How long each pose holds.
const BEAT: Duration = Duration::from_millis(200);

/// Quadrant blocks by which quarters are lit: upper left 1, upper right 2,
/// lower left 4, lower right 8.
const QUADS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

#[derive(Clone, Copy)]
enum Arm {
    Out,
    Up,
    Down,
}

impl Arm {
    /// The left arm's two pixels, from the body's top left corner. The right
    /// arm is the same mirrored.
    fn pixels(self) -> [(i32, i32); 2] {
        match self {
            Arm::Out => [(-2, 2), (-1, 2)],
            Arm::Up => [(-2, 0), (-1, 1)],
            Arm::Down => [(-1, 3), (-2, 4)],
        }
    }
}

#[derive(Clone, Copy)]
enum Feet {
    Both,
    /// The right pair lifted, the weight on the left.
    Left,
    Right,
}

struct Pose {
    /// The body's left edge; `HOME` is standing straight.
    x: i32,
    hop: bool,
    left: Arm,
    right: Arm,
    feet: Feet,
    /// A pixel either way the eyes turn.
    look: i32,
}

const fn pose(x: i32, hop: bool, left: Arm, right: Arm, feet: Feet, look: i32) -> Pose {
    Pose {
        x,
        hop,
        left,
        right,
        feet,
        look,
    }
}

#[rustfmt::skip]
const DANCE: [Pose; 8] = [
    pose(HOME,        false, Arm::Out,  Arm::Out,  Feet::Both,   0),
    pose(HOME,        true,  Arm::Out,  Arm::Out,  Feet::Both,   0),
    pose(HOME,        false, Arm::Out,  Arm::Out,  Feet::Both,   0),
    pose(HOME,        true,  Arm::Up,   Arm::Up,   Feet::Both,   0),
    pose(HOME - STEP, false, Arm::Up,   Arm::Down, Feet::Left,  -1),
    pose(HOME,        false, Arm::Out,  Arm::Out,  Feet::Both,  -1),
    pose(HOME + STEP, false, Arm::Down, Arm::Up,   Feet::Right,  1),
    pose(HOME,        false, Arm::Out,  Arm::Out,  Feet::Both,   1),
];

impl Pose {
    fn pixels(&self) -> [[bool; W]; H] {
        let mut px = [[false; W]; H];
        let mut set = |x: i32, y: i32| {
            if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
                && x < W
                && y < H
            {
                px[y][x] = true;
            }
        };
        // Feet stand on the bottom row; a hop lifts the body a step off it.
        let top = if self.hop { 0 } else { STEP };
        for row in 0..BODY_H {
            for col in 0..BODY_W {
                let eye = row == 1 && EYES.iter().any(|e| e + self.look == col);
                if !eye {
                    set(self.x + col, top + row);
                }
            }
        }
        for (dx, dy) in self.left.pixels() {
            set(self.x + dx, top + dy);
        }
        for (dx, dy) in self.right.pixels() {
            set(self.x + BODY_W - 1 - dx, top + dy);
        }
        let down = match self.feet {
            Feet::Both => [true, true],
            Feet::Left => [true, false],
            Feet::Right => [false, true],
        };
        // Feet on the ground stay where they are while the body sways over
        // them; in the air they go with it.
        let base = if self.hop { self.x } else { HOME };
        for (pair, down) in FEET.iter().zip(down) {
            if down {
                for dx in pair {
                    set(base + dx, top + BODY_H);
                }
            }
        }
        px
    }
}

/// Columns and rows Clawd takes at `scale`: 1 is the size Claude Code draws
/// it, 2 twice that.
pub fn size(scale: u16) -> (u16, u16) {
    (W as u16 * scale / 2, (H as u16 * scale).div_ceil(2))
}

/// Which pose of the dance it is now.
pub fn beat() -> usize {
    static START: OnceLock<Instant> = OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed();
    (t.as_millis() / BEAT.as_millis()) as usize % DANCE.len()
}

/// Pose `beat` at `scale`, row by row, with a space wherever Clawd is not.
pub fn render(scale: u16, beat: usize) -> Vec<String> {
    let px = DANCE[beat % DANCE.len()].pixels();
    let s = scale as usize;
    // An odd row of pixels leaves the bottom row of cells half empty.
    let lit = |x: usize, y: usize| px.get(y / s).is_some_and(|r| r[x / s]) as usize;
    let (cols, rows) = size(scale);
    (0..rows as usize)
        .map(|row| {
            (0..cols as usize)
                .map(|col| {
                    let (x, y) = (col * 2, row * 2);
                    QUADS[lit(x, y)
                        | (lit(x + 1, y) << 1)
                        | (lit(x, y + 1) << 2)
                        | (lit(x + 1, y + 1) << 3)]
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hop_with_arms_out_is_the_clawd_claude_code_draws() {
        assert_eq!(
            render(1, 1),
            ["  ▐▛███▜▌  ", " ▝▜█████▛▘ ", "   ▘▘ ▝▝   ", "           "]
        );
        // On the ground it is the same a cell lower.
        assert_eq!(
            render(1, 0),
            ["           ", "  ▐▛███▜▌  ", " ▝▜█████▛▘ ", "   ▘▘ ▝▝   "]
        );
    }

    #[test]
    fn no_pose_loses_a_pixel_off_the_edge() {
        for pose in &DANCE {
            let lit = pose.pixels().iter().flatten().filter(|&&p| p).count();
            let feet = match pose.feet {
                Feet::Both => 4,
                Feet::Left | Feet::Right => 2,
            };
            assert_eq!(lit, (BODY_W * BODY_H) as usize - 2 + 4 + feet);
        }
    }

    #[test]
    fn it_moves_on_every_beat() {
        for beat in 0..DANCE.len() {
            assert_ne!(render(1, beat), render(1, beat + 1), "{beat}");
        }
    }

    #[test]
    fn twice_the_size_is_whole_blocks() {
        let rows = render(2, 0);
        assert_eq!(rows.len(), 7);
        for row in rows {
            assert_eq!(row.chars().count(), 22);
            assert!(row.chars().all(|c| c == ' ' || c == '█'), "{row}");
        }
    }
}
