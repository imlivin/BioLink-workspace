// The exercise catalogue, grouped by functional category.
// Add, remove, or rename exercises here; the app picks up changes automatically.
const CATALOGUE = {
  Mobility: [
    'Arm rainbows',
  ],
  'Squat/Lunge': [
    'Ball squats',
    'Goblet squats',
    'Lunges',
    'Box squats',
  ],
  Push: [
    'Chest press',
    'Chest fly',
    'Incline press',
    'Cable crossovers',
    'Shoulder press',
    'Arnold press',
    'Front raises',
  ],
  Hinge: [
    'Romanian deadlifts',
    'Good mornings',
    'Kettlebell swings',
    'Deadlifts',
  ],
  Pull: [
    'Bent-over row',
    'Seated row',
    'Lat pulldown',
    'Resistance band rows',
    'TRX row',
    'Reverse fly',
    'Bicep curls',
    'Hammer curls',
    'Cable curls',
  ],
  Core: [
    'Bird dog',
    'Dead bug',
    'Torso rotations',
    'Pallof press',
  ],
};

const slug = (text) => text.toLowerCase().replace(/[^a-z0-9]+/g, '-');

// The database (per the Realtime Database rules) identifies exercises by an
// underscore slug drawn from a fixed whitelist, e.g. "Goblet squats" ->
// "goblet_squats", "Bent-over row" -> "bent_over_row".
const dbKey = (text) => text.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '');

// Sections in the shape SectionList expects: [{ title, data: [exercise] }]
export const SECTIONS = Object.entries(CATALOGUE).map(([category, names]) => ({
  title: category,
  data: names.map((name) => ({
    id: `${slug(category)}-${slug(name)}`,
    name,
    category,
    dbKey: dbKey(name),
  })),
}));
