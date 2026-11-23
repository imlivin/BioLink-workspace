//! Local PostgreSQL storage (tables are created/upgraded automatically).
//!
//! The phone only reports the moments start and stop are pressed, so:
//!   app_events  every /current change exactly as received: (ts, exercise), "none" = stop
//!   sessions    one row per start..stop: started_at = start press, stopped_at = stop press
//!               (NULL while running). id = start-press epoch ms.
//!   samples     raw sensor values recorded between start and stop, linked to their session
//!   samples_labeled (view)  samples joined with their session's exercise
//!
//! Nothing is stored for the time between a stop and the next start.

use crate::model::{Sample, SessionInfo};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use postgres::{Client, NoTls};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS app_events (
    ts        TIMESTAMPTZ NOT NULL,
    exercise  TEXT NOT NULL,
    PRIMARY KEY (ts, exercise)
);
CREATE TABLE IF NOT EXISTS sessions (
    id                BIGINT PRIMARY KEY,
    exercise          TEXT NOT NULL,
    started_at        TIMESTAMPTZ NOT NULL,
    stopped_at        TIMESTAMPTZ,
    last_sample_at    TIMESTAMPTZ,
    sample_count      BIGINT NOT NULL DEFAULT 0,
    accel_lsb_per_g   DOUBLE PRECISION NOT NULL,
    gyro_lsb_per_dps  DOUBLE PRECISION NOT NULL
);
CREATE TABLE IF NOT EXISTS samples (
    session_id  BIGINT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    ts          TIMESTAMPTZ NOT NULL,
    ax SMALLINT NOT NULL, ay SMALLINT NOT NULL, az SMALLINT NOT NULL,
    gx SMALLINT NOT NULL, gy SMALLINT NOT NULL, gz SMALLINT NOT NULL,
    PRIMARY KEY (session_id, ts)
);

-- upgrade tables created by earlier versions of this app
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = current_schema() AND table_name = 'sessions' AND column_name = 'ended_at') THEN
        ALTER TABLE sessions RENAME COLUMN ended_at TO last_sample_at;
    END IF;
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = current_schema() AND table_name = 'samples' AND column_name = 'exercise') THEN
        ALTER TABLE samples DROP COLUMN exercise;  -- the exercise lives on the session
    END IF;
END $$;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS stopped_at TIMESTAMPTZ;
ALTER TABLE sessions ALTER COLUMN last_sample_at DROP NOT NULL;

CREATE OR REPLACE VIEW samples_labeled AS
    SELECT s.ts, se.exercise, s.session_id, s.ax, s.ay, s.az, s.gx, s.gy, s.gz
    FROM samples s JOIN sessions se ON se.id = s.session_id;
"#;

pub struct Db {
    client: Client,
}

fn ts(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

const MS: &str = "(extract(epoch FROM {}) * 1000)::BIGINT";

fn ms_col(c: &str) -> String {
    MS.replace("{}", c)
}

impl Db {
    /// Connect, creating the database (if allowed) and the tables when missing.
    pub fn connect(url: &str) -> Result<Db> {
        let client = match Client::connect(url, NoTls) {
            Ok(c) => c,
            Err(e) if e.code() == Some(&postgres::error::SqlState::INVALID_CATALOG_NAME) => {
                let cfg: postgres::Config = url.parse().context("invalid postgres_url")?;
                let name = cfg.get_dbname().unwrap_or("biolink").to_string();
                let mut admin_cfg = cfg.clone();
                admin_cfg.dbname("postgres");
                let mut admin = admin_cfg
                    .connect(NoTls)
                    .context("connecting to the 'postgres' database to create it")?;
                admin
                    .batch_execute(&format!("CREATE DATABASE \"{}\"", name.replace('"', "")))
                    .with_context(|| format!("creating database {name}"))?;
                Client::connect(url, NoTls).context("connecting to the new database")?
            }
            Err(e) => return Err(anyhow::Error::new(e).context(
                "connecting to PostgreSQL (is the PostgreSQL service running? check postgres_url)",
            )),
        };
        let mut db = Db { client };
        db.client.batch_execute(SCHEMA).context("creating tables")?;
        Ok(db)
    }

    /// A start or stop press from the phone (`exercise` = None means stop).
    /// Logs the event, closes any running session at `at_ms`, and opens a new one on start.
    /// Safe to call again with the same event (e.g. after a restart).
    pub fn app_event(
        &mut self,
        at_ms: i64,
        raw_exercise: &str,
        start: Option<&SessionInfo>,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        tx.execute(
            "INSERT INTO app_events (ts, exercise) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            &[&ts(at_ms), &raw_exercise],
        )?;
        tx.execute(
            "UPDATE sessions SET stopped_at = $1 WHERE stopped_at IS NULL AND id < $2",
            &[&ts(at_ms), &at_ms],
        )?;
        if let Some(s) = start {
            tx.execute(
                "INSERT INTO sessions (id, exercise, started_at, accel_lsb_per_g, gyro_lsb_per_dps)
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO NOTHING",
                &[
                    &s.id,
                    &s.exercise,
                    &ts(s.started_ms),
                    &s.accel_lsb_per_g,
                    &s.gyro_lsb_per_dps,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Store samples for a running session. Returns the number of new rows.
    pub fn insert_samples(&mut self, session_id: i64, samples: &[Sample]) -> Result<u64> {
        if samples.is_empty() {
            return Ok(0);
        }
        let mut tx = self.client.transaction()?;
        let stmt = tx.prepare(
            "INSERT INTO samples (session_id, ts, ax, ay, az, gx, gy, gz)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
        )?;
        let mut n = 0u64;
        for s in samples {
            let r = s.raw;
            n += tx.execute(
                &stmt,
                &[
                    &session_id,
                    &ts(s.ts_ms),
                    &r[0],
                    &r[1],
                    &r[2],
                    &r[3],
                    &r[4],
                    &r[5],
                ],
            )?;
        }
        let last = samples
            .iter()
            .map(|s| s.ts_ms)
            .max()
            .unwrap_or(samples[0].ts_ms);
        tx.execute(
            "UPDATE sessions SET last_sample_at = GREATEST(last_sample_at, $2), sample_count = sample_count + $3 WHERE id = $1",
            &[&session_id, &ts(last), &(n as i64)],
        )?;
        tx.commit()?;
        Ok(n)
    }

    pub fn sessions(&mut self) -> Result<Vec<SessionInfo>> {
        let q = format!(
            "SELECT id, exercise, {}, {}, {}, sample_count, accel_lsb_per_g, gyro_lsb_per_dps FROM sessions ORDER BY id DESC",
            ms_col("started_at"),
            ms_col("stopped_at"),
            ms_col("last_sample_at")
        );
        let rows = self.client.query(&q, &[])?;
        Ok(rows
            .iter()
            .map(|r| SessionInfo {
                id: r.get(0),
                exercise: r.get(1),
                started_ms: r.get(2),
                stopped_ms: r.get(3),
                last_sample_ms: r.get(4),
                samples: r.get(5),
                accel_lsb_per_g: r.get(6),
                gyro_lsb_per_dps: r.get(7),
            })
            .collect())
    }

    pub fn samples(&mut self, session_id: i64) -> Result<Vec<Sample>> {
        let q = format!(
            "SELECT {}, ax, ay, az, gx, gy, gz FROM samples WHERE session_id = $1 ORDER BY ts",
            ms_col("ts")
        );
        let rows = self.client.query(&q, &[&session_id])?;
        Ok(rows
            .iter()
            .map(|r| Sample {
                ts_ms: r.get(0),
                raw: [r.get(1), r.get(2), r.get(3), r.get(4), r.get(5), r.get(6)],
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with: $env:BIOLINK_TEST_PG="postgresql://postgres:PASSWORD@localhost:5432/biolink"; cargo test -- --ignored
    #[test]
    #[ignore]
    fn round_trip() {
        let url = std::env::var("BIOLINK_TEST_PG").expect("set BIOLINK_TEST_PG");
        let mut db = Db::connect(&url).unwrap();
        let id = 1_000_000_000_123; // fixed test ids, removed again below
        let cleanup = |db: &mut Db| {
            db.client
                .execute(
                    "DELETE FROM sessions WHERE id IN ($1, $2)",
                    &[&id, &(id + 5000)],
                )
                .unwrap();
            db.client
                .execute(
                    "DELETE FROM app_events WHERE ts IN ($1, $2, $3)",
                    &[&ts(id), &ts(id + 5000), &ts(id + 9000)],
                )
                .unwrap();
        };
        cleanup(&mut db);
        let info = SessionInfo::new(id, "bicep_curls".into(), 16384.0, 131.0);
        db.app_event(id, "bicep_curls", Some(&info)).unwrap();
        let batch: Vec<Sample> = (0..48)
            .map(|i| Sample {
                ts_ms: id + 100 + i * 20,
                raw: [i as i16, 1, 2, 3, 4, -5],
            })
            .collect();
        assert_eq!(db.insert_samples(id, &batch).unwrap(), 48);
        assert_eq!(
            db.insert_samples(id, &batch).unwrap(),
            0,
            "duplicates are ignored"
        );
        // switching exercise closes the first session and opens a second
        let second = SessionInfo::new(id + 5000, "lunges".into(), 16384.0, 131.0);
        db.app_event(id + 5000, "lunges", Some(&second)).unwrap();
        db.app_event(id + 9000, "none", None).unwrap();
        let all = db.sessions().unwrap();
        let s1 = all.iter().find(|s| s.id == id).unwrap();
        let s2 = all.iter().find(|s| s.id == id + 5000).unwrap();
        assert_eq!(
            (s1.samples, s1.stopped_ms, s1.last_sample_ms),
            (48, Some(id + 5000), Some(id + 100 + 47 * 20))
        );
        assert_eq!(s2.stopped_ms, Some(id + 9000));
        assert_eq!(db.samples(id).unwrap(), batch);
        cleanup(&mut db);
    }
}
