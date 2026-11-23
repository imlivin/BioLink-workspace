//! Background workers.
//!
//! * Watchers: Firebase event streams on /test_imu and /current.
//! * Recorder: a /current change is a start press (exercise) or a stop press ("none"). It is
//!   logged and opens/closes a session. Each /test_imu update between start and stop is
//!   timestamped (20 ms apart) and stored in that session. Nothing is stored after a stop.
//! * DB worker: answers UI requests from PostgreSQL and computes per-session summaries.

use crate::analysis::{self, Analysis, Summary};
use crate::config::Config;
use crate::db::Db;
use crate::exercises;
use crate::firebase::Firebase;
use crate::model::{self, AppState, ImuSeries, Sample, SessionInfo};
use crate::stream;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum Command {
    Refresh,
    /// Load and analyse a session. `live` = for the live view (no raw-data view needed).
    Load { id: i64, live: bool },
}

/// A session with its analysis, computed off the UI thread.
pub struct LoadedSession {
    pub info: SessionInfo,
    pub series: ImuSeries,
    pub analysis: Analysis,
    pub raw: Option<crate::raw::RawView>,
}

#[derive(Clone, Default)]
pub struct LiveState {
    /// Last start/stop press received from the phone.
    pub app: Option<AppState>,
    /// Session currently being recorded (between a start and a stop press).
    pub open: Option<SessionInfo>,
    /// Local time (epoch ms) of the last /test_imu update, and its size.
    pub imu_seen_ms: Option<i64>,
    pub last_batch_len: usize,
}

pub enum Event {
    Sessions(Vec<SessionInfo>),
    Summary(i64, Summary),
    Loaded {
        session: Box<LoadedSession>,
        live: bool,
    },
    /// Samples were stored for this session.
    Recorded {
        session: i64,
    },
    /// A start/stop press created or closed a session.
    SessionsChanged,
    Live(LiveState),
    Status(String),
    Error(String),
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Exercise analysis of a stored session, plus notes about the data itself. `prev` is the
/// previous analysis of the same session while it was being recorded (keeps counted reps).
pub fn analyze_session(
    info: &SessionInfo,
    samples: &[Sample],
    prev: Option<&Analysis>,
) -> (ImuSeries, Analysis) {
    let series = model::build_series(info, samples);
    let ex = exercises::find_or_generic(&info.exercise);
    let mut a = analysis::analyze(&series, ex, Some(info.duration_s()), prev);
    let clip = model::clip_fraction(samples);
    if clip > 0.01 {
        a.summary.notes.insert(
            0,
            format!(
                "The sensor hit its limit in {:.0}% of samples (range ±{:.0} g / ±{:.0}°/s); ROM and speed are underestimated. A larger MPU-6050 range would fix this.",
                clip * 100.0,
                32768.0 / info.accel_lsb_per_g,
                32768.0 / info.gyro_lsb_per_dps
            ),
        );
    }
    if exercises::find(&info.exercise).is_none() {
        a.summary.notes.insert(0, format!("Unknown exercise '{}': analysed as forearm tilt.", info.exercise));
    }
    (series, a)
}

/// A start or stop press to write to the database.
#[derive(Debug, PartialEq)]
pub struct AppEvent {
    pub at_ms: i64,
    pub raw_exercise: String,
    /// The session opened by a start press (None for a stop press).
    pub start: Option<SessionInfo>,
}

/// Turns /current presses and /test_imu updates into sessions and timestamped samples.
pub struct Recorder {
    interval_ms: f64,
    accel_lsb_per_g: f64,
    gyro_lsb_per_dps: f64,
    open: Option<SessionInfo>,
    last_event: Option<(i64, Option<String>)>,
    last_ts: i64,
    /// Samples before this time are dropped: the start press, or when the PC received it if
    /// that is earlier (the phone clock may run ahead of the PC clock).
    cut_ms: i64,
}

impl Recorder {
    pub fn new(cfg: &Config) -> Self {
        Recorder {
            interval_ms: cfg.sample_interval_ms,
            accel_lsb_per_g: cfg.accel_lsb_per_g,
            gyro_lsb_per_dps: cfg.gyro_lsb_per_dps,
            open: None,
            last_event: None,
            last_ts: 0,
            cut_ms: 0,
        }
    }

    pub fn open(&self) -> Option<&SessionInfo> {
        self.open.as_ref()
    }

    /// /current changed (or was read on connect). Returns the event to store, or None if it
    /// is the same press seen again.
    pub fn on_app(&mut self, st: &AppState, now: i64) -> Option<AppEvent> {
        let at = st.timestamp_ms.unwrap_or(now);
        let key = (at, st.exercise.clone());
        if self.last_event.as_ref() == Some(&key) {
            return None;
        }
        self.last_event = Some(key);
        let start = st.exercise.as_ref().map(|ex| {
            SessionInfo::new(at, ex.clone(), self.accel_lsb_per_g, self.gyro_lsb_per_dps)
        });
        self.open = start.clone();
        self.last_ts = 0;
        self.cut_ms = at.min(now);
        Some(AppEvent {
            at_ms: at,
            raw_exercise: st.raw_exercise.clone(),
            start,
        })
    }

    /// A new /test_imu batch arrived at `arrival` (epoch ms). Returns (session id, samples) to
    /// store, or None when no exercise is running.
    pub fn on_batch(&mut self, raw: &[[i16; 6]], arrival: i64) -> Option<(i64, Vec<Sample>)> {
        let session = self.open.as_mut()?;
        if raw.is_empty() {
            return None;
        }
        let step = self.interval_ms;
        let cut = self.cut_ms;
        // the last sample of the batch was taken just before it arrived
        let mut start = arrival - ((raw.len() - 1) as f64 * step) as i64;
        if self.last_ts > 0 {
            start = start.max(self.last_ts + step as i64);
        }
        let samples: Vec<Sample> = raw
            .iter()
            .enumerate()
            .map(|(i, r)| Sample {
                ts_ms: start + (i as f64 * step) as i64,
                raw: *r,
            })
            .filter(|s| s.ts_ms >= cut) // only after the start press
            .collect();
        let last = samples.last()?.ts_ms;
        self.last_ts = last;
        session.last_sample_ms = Some(last);
        session.samples += samples.len() as i64;
        Some((session.id, samples))
    }
}

fn connect_retry(url: &str, tx: &Sender<Event>, ctx: &egui::Context) -> Db {
    loop {
        match Db::connect(url) {
            Ok(db) => return db,
            Err(e) => {
                let _ = tx.send(Event::Error(format!("{e:#}")));
                ctx.request_repaint();
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

enum Msg {
    Imu(Value, i64),
    App(Value, i64),
    Error(String),
}

pub fn spawn_recorder(fb: Arc<Firebase>, cfg: Config, tx: Sender<Event>, ctx: egui::Context) {
    let (mtx, mrx) = crossbeam_channel::unbounded::<Msg>();
    for (path, is_imu) in [
        (cfg.imu_path.clone(), true),
        (cfg.app_state_path.clone(), false),
    ] {
        let (fb, mtx) = (fb.clone(), mtx.clone());
        std::thread::spawn(move || {
            let etx = mtx.clone();
            stream::watch(
                &fb,
                &path,
                |v, snapshot| {
                    let msg = if is_imu {
                        // the snapshot on connect is an old batch: don't store it
                        if snapshot {
                            return;
                        }
                        Msg::Imu(v.clone(), now_ms())
                    } else {
                        // the snapshot is the latest press, which is still in effect
                        Msg::App(v.clone(), now_ms())
                    };
                    let _ = mtx.send(msg);
                },
                |e| {
                    let _ = etx.send(Msg::Error(format!("/{path}: {e}")));
                },
            )
        });
    }

    std::thread::spawn(move || {
        let mut db = connect_retry(&cfg.postgres_url, &tx, &ctx);
        let mut rec = Recorder::new(&cfg);
        let send = |e: Event| {
            let _ = tx.send(e);
            ctx.request_repaint();
        };
        // /test_imu may arrive as several events per update; store it once things go quiet
        let mut pending: Option<(Value, i64)> = None;
        let mut last_change = Instant::now();
        let mut previous: Vec<[i16; 6]> = vec![];
        let mut live = LiveState::default();
        let mut last_live = Instant::now();
        loop {
            match mrx.recv_timeout(Duration::from_millis(40)) {
                Ok(Msg::Imu(v, at)) => {
                    pending = Some((v, at));
                    last_change = Instant::now();
                }
                Ok(Msg::App(v, at)) => {
                    let st = AppState::parse(&v);
                    if let Some(ev) = rec.on_app(&st, at) {
                        let mut res = db.app_event(ev.at_ms, &ev.raw_exercise, ev.start.as_ref());
                        if res.is_err() {
                            db = connect_retry(&cfg.postgres_url, &tx, &ctx);
                            res = db.app_event(ev.at_ms, &ev.raw_exercise, ev.start.as_ref());
                        }
                        match res {
                            Ok(()) => send(Event::SessionsChanged),
                            Err(e) => send(Event::Error(format!(
                                "Saving start/stop to PostgreSQL failed: {e:#}"
                            ))),
                        }
                    }
                    live.app = Some(st);
                }
                Ok(Msg::Error(e)) => send(Event::Error(e)),
                Err(_) => {}
            }
            if pending.is_some() && last_change.elapsed() >= Duration::from_millis(120) {
                let (v, at) = pending.take().unwrap_or_default();
                let raw = model::parse_imu_batch(&v);
                live.imu_seen_ms = Some(at);
                live.last_batch_len = raw.len();
                if !raw.is_empty() && raw != previous {
                    if let Some((session, samples)) = rec.on_batch(&raw, at) {
                        match db.insert_samples(session, &samples) {
                            Ok(_) => send(Event::Recorded { session }),
                            Err(e) => {
                                send(Event::Error(format!(
                                    "Saving samples to PostgreSQL failed: {e:#}"
                                )));
                                db = connect_retry(&cfg.postgres_url, &tx, &ctx);
                            }
                        }
                    }
                    previous = raw;
                }
            }
            if last_live.elapsed() >= Duration::from_millis(500) {
                last_live = Instant::now();
                live.open = rec.open().cloned();
                send(Event::Live(live.clone()));
            }
        }
    });
}

pub fn spawn_db_worker(
    pg_url: String,
    rx: Receiver<Command>,
    tx: Sender<Event>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let mut db = connect_retry(&pg_url, &tx, &ctx);
        let send = |e: Event| {
            let _ = tx.send(e);
            ctx.request_repaint();
        };
        let mut sessions: Vec<SessionInfo> = vec![];
        // session id -> sample count the summary was computed with
        let mut summarized: HashMap<i64, i64> = HashMap::new();
        // Latest analysis of sessions recorded while the app was running. Later analyses
        // build on it, so reps that were shown once are never taken away.
        let mut recorded: HashMap<i64, Analysis> = HashMap::new();
        let mut refresh = true;
        let mut queue: std::collections::VecDeque<Command> = Default::default();
        loop {
            let cmd = if refresh {
                Some(Command::Refresh)
            } else if let Some(c) = queue.pop_front() {
                Some(c)
            } else {
                // take everything waiting, keeping only the newest load of each kind
                let first = rx.recv_timeout(Duration::from_millis(200)).ok();
                let mut pending: Vec<Command> = first.into_iter().chain(rx.try_iter()).collect();
                let mut seen = (false, false);
                pending.reverse();
                pending.retain(|c| match c {
                    Command::Load { live, .. } => {
                        let slot = if *live { &mut seen.0 } else { &mut seen.1 };
                        !std::mem::replace(slot, true)
                    }
                    Command::Refresh => true,
                });
                pending.reverse();
                queue.extend(pending);
                queue.pop_front()
            };
            refresh = false;
            match cmd {
                Some(Command::Refresh) => match db.sessions() {
                    Ok(list) => {
                        sessions = list;
                        send(Event::Sessions(sessions.clone()));
                        send(Event::Status(format!(
                            "{} sessions in PostgreSQL",
                            sessions.len()
                        )));
                    }
                    Err(e) => {
                        send(Event::Error(format!("PostgreSQL: {e:#}")));
                        db = connect_retry(&pg_url, &tx, &ctx);
                    }
                },
                Some(Command::Load { id, live }) => {
                    let info = db
                        .sessions()
                        .ok()
                        .and_then(|l| l.into_iter().find(|s| s.id == id));
                    match (info, db.samples(id)) {
                        (Some(info), Ok(samples)) => {
                            let start = Analysis::recording_start();
                            let prev = recorded
                                .get(&id)
                                .or(info.in_progress().then_some(&start));
                            let (series, analysis) = analyze_session(&info, &samples, prev);
                            if prev.is_some() {
                                recorded.insert(id, analysis.clone());
                            }
                            let raw = (!live).then(|| crate::raw::compute(&info, &samples));
                            let session = LoadedSession { info, series, analysis, raw };
                            send(Event::Loaded {
                                session: Box::new(session),
                                live,
                            })
                        }
                        (None, _) => send(Event::Error(format!("Session {id} not found"))),
                        (_, Err(e)) => send(Event::Error(format!("Loading samples failed: {e:#}"))),
                    }
                }
                None => {
                    // idle: summarize one finished session that hasn't been summarized yet
                    if let Some(info) = sessions
                        .iter()
                        .find(|s| !s.in_progress() && summarized.get(&s.id) != Some(&s.samples))
                        .cloned()
                    {
                        summarized.insert(info.id, info.samples);
                        if let Ok(samples) = db.samples(info.id) {
                            let prev = recorded.get(&info.id);
                            send(Event::Summary(
                                info.id,
                                analyze_session(&info, &samples, prev).1.summary,
                            ));
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg() -> Config {
        toml::from_str("database_url='x'\napi_key='k'\npostgres_url='p'").unwrap()
    }

    fn press(r: &mut Recorder, exercise: &str, at: i64) -> Option<AppEvent> {
        r.on_app(
            &AppState::parse(&json!({"exercise": exercise, "timestamp": at})),
            at,
        )
    }

    #[test]
    fn start_stop_presses_define_sessions() {
        let mut r = Recorder::new(&cfg());
        let batch = vec![[0i16; 6]; 48];
        let t = 1_762_790_000_000;
        // before any start press: nothing stored
        assert!(r.on_batch(&batch, t).is_none());
        // start
        let ev = press(&mut r, "bicep_curls", t).unwrap();
        assert_eq!(ev.start.as_ref().map(|s| s.id), Some(t));
        // the same press seen again (e.g. reconnect) is ignored
        assert!(press(&mut r, "bicep_curls", t).is_none());
        // a batch arriving 0.5 s after start: samples from before the press are dropped
        let (id, a) = r.on_batch(&batch, t + 500).unwrap();
        assert_eq!(id, t);
        assert!(a.iter().all(|s| s.ts_ms >= t));
        assert_eq!(a.len(), 26);
        let (_, b) = r.on_batch(&batch, t + 1500).unwrap();
        assert_eq!(b.len(), 48);
        assert!(b[0].ts_ms > a[a.len() - 1].ts_ms);
        assert_eq!(b[1].ts_ms - b[0].ts_ms, 20);
        // stop
        let stop = press(&mut r, "none", t + 2000).unwrap();
        assert!(stop.start.is_none());
        assert_eq!(stop.raw_exercise, "none");
        assert!(r.on_batch(&batch, t + 3000).is_none());
    }
}
