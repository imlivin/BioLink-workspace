// Data layer for the Firebase Realtime Database.
//
// A single node "current" with exactly two fields:
//   - exercise:  the exercise slug while running, or "none" when stopped
//   - timestamp: ms since epoch, noted every time a button is pressed
//
// Start -> { exercise: <slug>, timestamp: now }
// Stop  -> { exercise: "none",  timestamp: now }
import { ref, set } from 'firebase/database';
import { signInWithEmailAndPassword } from 'firebase/auth';
import { auth, db } from './firebase';

// Service account the rules authorize for app writes.
const APP_EMAIL = 'app@biolinkdb.example';
const APP_PASSWORD = 'admin12';

const CURRENT = 'current';

let signInPromise = null;

/** Sign in (once) as the app service account; reuse the result afterwards. */
export function ensureSignedIn() {
  if (auth.currentUser) return Promise.resolve(auth.currentUser);
  if (!signInPromise) {
    signInPromise = signInWithEmailAndPassword(auth, APP_EMAIL, APP_PASSWORD)
      .then((cred) => cred.user)
      .catch((err) => {
        signInPromise = null; // allow a retry next time
        throw err;
      });
  }
  return signInPromise;
}

/**
 * Record the exercise (and timestamp) when Start is pressed.
 * @param {string} exerciseKey - the exercise slug, e.g. "goblet_squats"
 */
export async function recordExercise(exerciseKey) {
  await ensureSignedIn();
  await set(ref(db, CURRENT), {
    exercise: exerciseKey,
    timestamp: Date.now(),
  });
}

/** Record "none" (and timestamp) when Stop is pressed. */
export async function recordStop() {
  await ensureSignedIn();
  await set(ref(db, CURRENT), {
    exercise: 'none',
    timestamp: Date.now(),
  });
}
