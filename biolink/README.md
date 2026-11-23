# BioLink: wrist IMU exercise tracker (prototype)

A Windows desktop app (Rust) for trainers. It records the accelerometer and gyro data the ESP32 on the
client's right wrist sends to Firebase, between the mobile app's start and stop presses. It stores the data in
a local PostgreSQL database and shows it two ways: an exercise analysis (reps, tempo, quality) and a raw-data
view that doesn't depend on the exercise.

```
desktop/    Rust desktop app (biolink.exe)
firebase/   database.rules.json (copy of the Firebase rules, for reference)
```

## Data flow

```
ESP32 ──(overwrites once per second)──> Firebase /test_imu ─┐
                                                            ├──> desktop app ──> PostgreSQL (local)
phone ──(on start / stop press)───────> Firebase /current ──┘
```

- **`/test_imu`** (ESP32): indexes 0–47, 20 ms apart, overwritten once per second with raw MPU-6050 counts
  `{"ax", "ay", "az", "gx", "gy", "gz"}`. The scale is set in `biolink.toml` (defaults ±2 g, ±250 °/s).
- **`/current`** (phone, written only when start or stop is pressed): `{"exercise": "bicep_curls", "timestamp": <epoch ms>}`
  on start, `{"exercise": "none", ...}` on stop.
- A start press opens a session at the phone's timestamp. Each `/test_imu` update after that is timestamped
  (the last sample = when the update arrived, earlier samples 20 ms apart) and saved. A stop press closes the
  session, and nothing is saved until the next start. Pressing start for a new exercise without stopping first
  closes the previous session.

## Setup

1. **PostgreSQL**: install it (port 5432) and remember the `postgres` password. The app creates the `biolink`
   database and its tables on first start.
2. **Firebase console → Authentication → Sign-in method**: enable **Anonymous**. The desktop app uses it to read.
3. **Config**:
   ```powershell
   cd desktop
   copy config.example.toml biolink.toml
   ```
   Then set `api_key` and the password in `postgres_url`.
4. **Run**:
   ```powershell
   cargo run --release
   ```
   The executable is `desktop\target\release\biolink.exe`. Keep `biolink.toml` next to it, or in `%APPDATA%\BioLink\`.

Keep the desktop app open while exercising. `/test_imu` holds only the latest second, so the app has to be
running to capture it.

## PostgreSQL

- **Check it's running:** `Get-Service postgresql-x64-18` in PowerShell. Its startup type is Automatic, so it
  starts with Windows.
- **Start it if it's stopped:** `Start-Service postgresql-x64-18`, in PowerShell run as administrator.
- **Browse the data:** open **pgAdmin 4** from the Start menu. Go to Servers → PostgreSQL 18 → Databases →
  biolink → Schemas → public → Tables. To run queries, right-click `biolink` → Query Tool.
- **Command line:**
  ```powershell
  & "C:\Program Files\PostgreSQL\18\bin\psql.exe" -U postgres -d biolink
  ```
  Then run `\dt` to list tables and `\q` to quit.

The tables (created and upgraded automatically):

| Table | Contents |
|---|---|
| `app_events` | Every start/stop press exactly as received: `ts`, `exercise` (`"none"` = stop) |
| `sessions` | One row per start → stop: `id` (start time in epoch ms), `exercise`, `started_at`, `stopped_at` (empty while running), `last_sample_at`, `sample_count`, sensor scale |
| `samples` | Raw values between start and stop: `session_id`, `ts`, `ax`, `ay`, `az`, `gx`, `gy`, `gz` |
| `samples_labeled` | View: `samples` with each session's exercise added |

```sql
SELECT exercise, started_at, stopped_at, stopped_at - started_at AS elapsed, sample_count FROM sessions ORDER BY id DESC;
SELECT * FROM samples_labeled WHERE exercise = 'bicep_curls' ORDER BY ts LIMIT 100;
```

## Tabs

Graph controls: the mouse wheel over a graph zooms the time axis around the cursor (Shift + wheel zooms the
vertical axis). Drag to pan, and double-click to reset. Time graphs on the same page stay lined up when you zoom
or pan one of them. The page scrolls only when the cursor isn't over a graph.

- **Exercise analysis**: reps, sets and rest, range of motion, lift/lower times, time under tension, peak speed,
  quality scores and coaching notes.
  Graphs show the movement with each rep shaded and per-rep bars. There are also set and rep tables.
- **Raw data** (independent of the exercise):
  - Acceleration and rotation speed per axis, with magnitudes.
  - Tilt from gravity.
  - Integrated rotation, and vertical acceleration → speed → height change.
  - Frequency content (movement rhythm).
  - Per-axis statistics, plus summary cards: % time moving, peak values, total rotation, vertical range, and a rhythm/cycle estimate.
- **Progress**: trends across sessions for one exercise.
- **Live**: sensor and phone status, the running session with its start time and elapsed time, a live rep count,
  and the last 30 s of data.

## Tests

```powershell
cd desktop
cargo test                                            # unit tests
$env:BIOLINK_TEST_PG="postgresql://postgres:PASSWORD@localhost:5432/biolink"
cargo test -- --ignored                               # PostgreSQL round trip (uses and removes fixed test rows)
```

## How the exercise analysis works

1. **Cleaning.** Single-sample glitches (one reading far from both neighbours, such as `az` dropping to 0 g)
   are replaced. Gaps longer than 0.25 s are treated as "no movement" and marked, instead of being
   interpolated, so a Wi-Fi gap can't turn into a fake rep. The gyro offset is measured from the stillest second.
2. **Which way is down.** A complementary filter (gyro + accelerometer) tracks the direction of gravity
   relative to the sensor, so the way the board is mounted doesn't matter.
3. **Tracking signal.** Five signals are computed from the wrist:
   - **Forearm tilt** relative to gravity (curls, raises, swings, hinges...).
   - **Arm rotation** around its main axis, from the gyro.
   - **Vertical travel** and **linear travel** (estimated cm, from the accelerometer).
   - **Rotation about vertical** (torso rotations).

   The one that repeats most regularly (autocorrelation) is used. The exercise's usual signal gets a
   head start of 0.15, so another signal only wins when it is clearly better, and the session notes say so.
   The autocorrelation peak also gives the **rep period**.
4. **Reps = movement peaks.** Each peak in the signal is one rep, if it is at least about half a period
   after the previous one and stands out at least 40% as much as a typical rep (and at least 12 cm or 15°).
   A rep runs from where the movement leaves the low point (10% of the excursion) to where it returns.
   If the recording starts mid-rep, that first rep is counted but left out of the scores. Reps with more
   than 40% of their data missing are dropped.
5. **Live counting.** While a session is recorded, counting starts once two reps are recognised (about 8 s),
   then the chosen signal and period are kept and reps already shown are never removed, so the count only goes up.
   After the stop press the app keeps that result for the session.
6. **Sets.** Reps more than about 8 s apart start a new set. Each set reports rest time, pace (time between
   peaks), speed loss and ROM drop (fatigue).
7. **Scores (0–100):**
   - **Consistency** (40%): variation in ROM and in the time between movement peaks. Variation is measured
     with the median absolute deviation, so one odd rep next to a data gap doesn't dominate.
   - **Form** (30%): how closely each rep's shape matches the session's typical rep.
   - **Tempo** (30%): full marks for reps inside the target time (1.5–6 s for strength exercises), falling to
     0 at half the lower limit or 1.5× the upper one. A lowering phase much quicker than the lift costs a quarter.

   **Quality** is the weighted average.

Checked against the 28 test sessions recorded on 22 Nov (10–12 reps each): most exercises give 9–12 reps.
Lower counts come from data gaps (Romanian deadlifts, front raises) or sessions cut short.
The skipped exercises use the settings of a similar recorded one.
Bird dog and dead bug count right-arm reps only. Distances in cm are estimates. If more than 1% of readings
hit the sensor's ±2 g / ±250 °/s limit, the app adds a warning note.