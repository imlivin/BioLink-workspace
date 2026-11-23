//! Exercise catalog: which wrist signal each exercise is tracked with, and its tempo limits.
//!
//! Exercises from the original list that a single right-wrist IMU cannot track reliably
//! (standing hip opener/closer, step-ups) are intentionally left out.

/// Which motion of the wrist carries the repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Forearm tilt relative to gravity (curls, raises, presses, rows, swings...). Unit: degrees.
    Tilt,
    /// Up/down travel of the wrist (squats, lunges, deadlifts). Unit: cm (estimated).
    Vertical,
    /// Turning about the vertical axis (torso rotations). Unit: degrees.
    Rotation,
    /// Straight-line travel along the dominant direction while the forearm angle stays
    /// roughly constant (seated row, band row, Pallof press). Unit: cm (estimated).
    Linear,
    /// Turning of the forearm about its main rotation axis, from the gyroscope (degrees).
    /// Used when the gravity-based tilt barely changes (e.g. lunges, presses).
    Swing,
}

impl Mode {
    pub const ALL: [Mode; 5] = [Mode::Tilt, Mode::Vertical, Mode::Rotation, Mode::Linear, Mode::Swing];

    pub fn unit(self) -> &'static str {
        match self {
            Mode::Tilt | Mode::Rotation | Mode::Swing => "°",
            Mode::Vertical | Mode::Linear => "cm",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Tilt => "Forearm tilt",
            Mode::Vertical => "Vertical travel",
            Mode::Rotation => "Rotation",
            Mode::Linear => "Linear travel",
            Mode::Swing => "Arm rotation",
        }
    }

    /// Smallest movement that still counts as a rep. Real squats moved the wrist 35-60 cm;
    /// handling the sensor produced 6-8 cm.
    pub fn min_rom(self) -> f64 {
        match self {
            Mode::Tilt | Mode::Rotation | Mode::Swing => 15.0,
            Mode::Vertical | Mode::Linear => 12.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Exercise {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub mode: Mode,
    /// Shortest plausible rep (s); also the minimum spacing between detected reps.
    pub min_rep_s: f64,
    /// Rep duration considered well-controlled (s).
    pub target_rep_s: (f64, f64),
    /// Low-pass cutoff for the rep signal (Hz).
    pub lowpass_hz: f64,
    /// Some(true): moving away from the start position is the lifting (concentric) phase.
    /// Some(false): moving away is the lowering (eccentric) phase. None: not meaningful.
    pub out_is_concentric: Option<bool>,
    /// Shown to the trainer to explain what a counted rep means.
    pub rep_note: &'static str,
}

const fn ex(
    id: &'static str,
    name: &'static str,
    category: &'static str,
    mode: Mode,
    min_rep_s: f64,
    target_rep_s: (f64, f64),
    lowpass_hz: f64,
    out_is_concentric: Option<bool>,
    rep_note: &'static str,
) -> Exercise {
    Exercise {
        id,
        name,
        category,
        mode,
        min_rep_s,
        target_rep_s,
        lowpass_hz,
        out_is_concentric,
        rep_note,
    }
}

use Mode::*;

/// Controlled strength rep: 1.5-6 s from start to finish. Recorded reps at a normal pace took 1.5-3 s.
const STRENGTH: (f64, f64) = (1.5, 6.0);
const NOTE: &str = "1 rep = out and back";
const EACH_SIDE: &str = "Right wrist only: 1 rep = one left + one right";

pub const CATALOG: &[Exercise] = &[
    // Mobility
    ex(
        "arm_rainbows",
        "Arm rainbows",
        "Mobility",
        Tilt,
        0.8,
        (1.5, 6.0),
        1.5,
        None,
        "1 rep = one sweep over and back",
    ),
    // Squat / Lunge
    ex(
        "ball_squats",
        "Ball squats",
        "Squat/Lunge",
        Vertical,
        1.2,
        STRENGTH,
        1.5,
        Some(false),
        NOTE,
    ),
    ex(
        "goblet_squats",
        "Goblet squats",
        "Squat/Lunge",
        Vertical,
        1.2,
        STRENGTH,
        1.5,
        Some(false),
        NOTE,
    ),
    ex(
        "lunges",
        "Lunges",
        "Squat/Lunge",
        Vertical,
        1.2,
        STRENGTH,
        1.5,
        Some(false),
        "1 rep = one lunge (either leg)",
    ),
    ex(
        "box_squats",
        "Box squats",
        "Squat/Lunge",
        Vertical,
        1.2,
        STRENGTH,
        1.5,
        Some(false),
        NOTE,
    ),
    // Push
    ex(
        "chest_press",
        "Chest press",
        "Push",
        Vertical,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "chest_fly",
        "Chest fly",
        "Push",
        Tilt,
        1.0,
        STRENGTH,
        2.0,
        Some(false),
        NOTE,
    ),
    ex(
        "incline_press",
        "Incline press",
        "Push",
        Vertical,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "cable_crossovers",
        "Cable crossovers",
        "Push",
        Tilt,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "shoulder_press",
        "Shoulder press",
        "Push",
        Vertical,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "arnold_press",
        "Arnold press",
        "Push",
        Vertical,
        1.2,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "front_raises",
        "Front raises",
        "Push",
        Tilt,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    // Hinge
    ex(
        "romanian_deadlifts",
        "Romanian deadlifts",
        "Hinge",
        Vertical,
        1.2,
        STRENGTH,
        1.5,
        Some(false),
        NOTE,
    ),
    ex(
        "good_mornings",
        "Good mornings",
        "Hinge",
        Tilt,
        1.2,
        STRENGTH,
        1.5,
        Some(false),
        NOTE,
    ),
    ex(
        "kettlebell_swings",
        "Kettlebell swings",
        "Hinge",
        Tilt,
        0.7,
        (0.9, 2.5),
        2.5,
        None,
        "1 rep = one swing",
    ),
    ex(
        "deadlifts",
        "Deadlifts",
        "Hinge",
        Vertical,
        1.2,
        STRENGTH,
        1.5,
        Some(true),
        NOTE,
    ),
    // Pull
    ex(
        "bent_over_row",
        "Bent-over row",
        "Pull",
        Vertical,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "seated_row",
        "Seated row",
        "Pull",
        Linear,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "lat_pulldown",
        "Lat pulldown",
        "Pull",
        Vertical,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "resistance_band_rows",
        "Resistance band rows",
        "Pull",
        Linear,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "trx_row",
        "TRX row",
        "Pull",
        Linear,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "reverse_fly",
        "Reverse fly",
        "Pull",
        Tilt,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "bicep_curls",
        "Bicep curls",
        "Pull",
        Tilt,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "hammer_curls",
        "Hammer curls",
        "Pull",
        Tilt,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    ex(
        "cable_curls",
        "Cable curls",
        "Pull",
        Tilt,
        1.0,
        STRENGTH,
        2.0,
        Some(true),
        NOTE,
    ),
    // Core
    ex(
        "bird_dog",
        "Bird dog",
        "Core",
        Tilt,
        1.5,
        (2.5, 10.0),
        1.5,
        None,
        EACH_SIDE,
    ),
    ex(
        "dead_bug",
        "Dead bug",
        "Core",
        Tilt,
        1.5,
        (2.5, 10.0),
        1.5,
        None,
        EACH_SIDE,
    ),
    ex(
        "torso_rotations",
        "Torso rotations",
        "Core",
        Rotation,
        1.2,
        (2.0, 8.0),
        1.5,
        None,
        "1 rep = left + right",
    ),
    ex(
        "pallof_press",
        "Pallof press",
        "Core",
        Linear,
        1.2,
        (2.0, 10.0),
        1.5,
        None,
        NOTE,
    ),
];

pub const CATEGORIES: &[&str] = &["Mobility", "Squat/Lunge", "Push", "Hinge", "Pull", "Core"];

/// Used for exercise names the catalog doesn't know; the user can pick the tracking signal.
pub const GENERIC: Exercise = ex(
    "unknown",
    "Unknown exercise",
    "Other",
    Tilt,
    1.0,
    STRENGTH,
    2.0,
    None,
    NOTE,
);

pub fn find(id: &str) -> Option<&'static Exercise> {
    CATALOG.iter().find(|e| e.id == id)
}

pub fn find_or_generic(id: &str) -> &'static Exercise {
    find(id).unwrap_or(&GENERIC)
}

/// Display name for any exercise id, including unknown ones.
pub fn name_of(id: &str) -> String {
    find(id)
        .map(|e| e.name.to_string())
        .unwrap_or_else(|| id.replace('_', " "))
}
