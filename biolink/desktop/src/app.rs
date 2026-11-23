use biolink::analysis::{Analysis, Rep, Summary};
use biolink::config;
use biolink::db::Db;
use biolink::exercises;
use biolink::firebase::Firebase;
use biolink::model::SessionInfo;
use biolink::store::{self, Command, Event, LiveState};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Color32, RichText};
use egui_plot::{
    Bar, BarChart, HLine, Legend, Line, MarkerShape, Plot, PlotPoint, PlotPoints, Points, Span,
    Text,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const GREEN: Color32 = Color32::from_rgb(70, 190, 110);
const RED: Color32 = Color32::from_rgb(230, 90, 80);
const AMBER: Color32 = Color32::from_rgb(230, 180, 60);
const BLUE: Color32 = Color32::from_rgb(90, 140, 255);
const ORANGE: Color32 = Color32::from_rgb(255, 160, 60);
const GREY: Color32 = Color32::from_rgb(170, 170, 170);

/// Loading and analysis happen on the database worker thread, so the UI never blocks.
type Loaded = store::LoadedSession;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Session,
    Raw,
    Progress,
    Live,
}

#[derive(PartialEq, Clone, Copy)]
enum RepMetric {
    Rom,
    Duration,
    Phases,
    Similarity,
    Speed,
}

impl RepMetric {
    const ALL: [RepMetric; 5] = [
        RepMetric::Rom,
        RepMetric::Duration,
        RepMetric::Phases,
        RepMetric::Similarity,
        RepMetric::Speed,
    ];
    fn label(self) -> &'static str {
        match self {
            RepMetric::Rom => "Range of motion",
            RepMetric::Duration => "Rep time",
            RepMetric::Phases => "Phase times",
            RepMetric::Similarity => "Form similarity",
            RepMetric::Speed => "Peak speed",
        }
    }
}

pub struct BioApp {
    tx: Option<Sender<Command>>,
    rx: Option<Receiver<Event>>,
    setup_error: Option<String>,
    sessions: Vec<SessionInfo>,
    summaries: HashMap<i64, Summary>,
    selected: Option<i64>,
    loaded: Option<Loaded>,
    loading: bool,
    tab: Tab,
    filter_cat: Option<&'static str>,
    filter_ex: Option<String>,
    progress_ex: Option<String>,
    live: LiveState,
    live_id: Option<i64>,
    live_view: Option<Loaded>,
    last_import: Option<Instant>,
    status: String,
    error: Option<(String, Instant)>,
    rep_metric: RepMetric,
}

const LINK: &str = "session_x";

impl BioApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut app = BioApp {
            tx: None,
            rx: None,
            setup_error: None,
            sessions: vec![],
            summaries: HashMap::new(),
            selected: None,
            loaded: None,
            loading: false,
            tab: Tab::Session,
            filter_cat: None,
            filter_ex: None,
            progress_ex: None,
            live: LiveState::default(),
            live_id: None,
            live_view: None,
            last_import: None,
            status: "Connecting…".into(),
            error: None,
            rep_metric: RepMetric::Rom,
        };
        if let Err(e) = app.start(ctx) {
            app.setup_error = Some(format!("{e:#}"));
        }
        app
    }

    fn start(&mut self, ctx: &egui::Context) -> anyhow::Result<()> {
        let (cfg, _path) = config::load()?;
        // check PostgreSQL once up front (creates the database and tables if missing)
        Db::connect(&cfg.postgres_url)?;
        let fb = Arc::new(Firebase::new(
            &cfg,
            config::data_dir().join("viewer_token.txt"),
        )?);
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        store::spawn_db_worker(cfg.postgres_url.clone(), cmd_rx, ev_tx.clone(), ctx.clone());
        store::spawn_recorder(fb, cfg, ev_tx, ctx.clone());
        self.tx = Some(cmd_tx);
        self.rx = Some(ev_rx);
        Ok(())
    }

    fn send(&self, c: Command) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(c);
        }
    }

    fn select(&mut self, id: i64) {
        if self.selected != Some(id) {
            self.loaded = None;
        }
        self.selected = Some(id);
        self.loading = true;
        self.request_selected();
    }

    /// (Re)load the selected session.
    fn request_selected(&self) {
        let Some(id) = self.selected else { return };
        self.send(Command::Load { id, live: false });
    }

    fn request_live(&self, id: i64) {
        self.send(Command::Load { id, live: true });
    }

    /// Exercise currently running (between the phone's start and stop press).
    fn app_exercise(&self) -> Option<String> {
        self.live.open.as_ref().map(|s| s.exercise.clone())
    }

    /// The sensor updated /test_imu in the last few seconds.
    fn sensor_online(&self) -> bool {
        self.live
            .imu_seen_ms
            .is_some_and(|t| now_ms() - (t as f64) < 5_000.0)
    }

    /// Samples were stored in the last few seconds.
    fn recording(&self) -> bool {
        self.last_import
            .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
    }

    fn poll_events(&mut self) {
        let Some(rx) = self.rx.clone() else { return };
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Event::Sessions(list) => {
                    self.sessions = list;
                    if self.selected.is_none() {
                        if let Some(id) = self.sessions.first().map(|s| s.id) {
                            self.select(id);
                        }
                    }
                }
                Event::Summary(id, s) => {
                    self.summaries.insert(id, s);
                }
                Event::Loaded { session, live } => {
                    let l = *session;
                    if live {
                        if self.live_id == Some(l.info.id) {
                            self.live_view = Some(l);
                        }
                    } else if self.selected == Some(l.info.id) {
                        if !l.info.in_progress() {
                            self.summaries.insert(l.info.id, l.analysis.summary.clone());
                        }
                        self.loaded = Some(l);
                        self.loading = false;
                    }
                }
                Event::Recorded { session } => {
                    let first_batch = self.live_id != Some(session);
                    self.last_import = Some(Instant::now());
                    self.live_id = Some(session);
                    self.request_live(session);
                    if first_batch {
                        self.send(Command::Refresh);
                    } else if self.selected == Some(session) {
                        self.request_selected();
                    }
                }
                Event::SessionsChanged => {
                    self.send(Command::Refresh);
                    self.request_selected();
                }
                Event::Live(state) => self.live = state,
                Event::Status(s) => self.status = s,
                Event::Error(e) => self.error = Some((e, Instant::now())),
            }
        }
    }
}

// ---------- formatting helpers ----------
fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

fn fmt_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%a %d %b %Y  %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn fmt_short_date(ms: f64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|d| d.with_timezone(&chrono::Local).format("%d %b").to_string())
        .unwrap_or_default()
}

fn fmt_dur(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_opt(v: Option<f64>, f: impl Fn(f64) -> String) -> String {
    v.map(f).unwrap_or_else(|| "–".into())
}

fn score_color(v: f64) -> Color32 {
    if v >= 80.0 {
        GREEN
    } else if v >= 60.0 {
        AMBER
    } else {
        RED
    }
}

fn card(
    ui: &mut egui::Ui,
    title: &str,
    value: String,
    sub: Option<String>,
    color: Option<Color32>,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_min_width(118.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(title).small().weak());
            let mut t = RichText::new(value).size(22.0).strong();
            if let Some(c) = color {
                t = t.color(c);
            }
            ui.label(t);
            if let Some(s) = sub {
                ui.label(RichText::new(s).small());
            }
        });
    });
}

fn score_card(ui: &mut egui::Ui, title: &str, v: Option<f64>, sub: &str) {
    card(
        ui,
        title,
        fmt_opt(v, |x| format!("{x:.0}")),
        Some(sub.to_string()),
        v.map(score_color),
    );
}

fn decimate(t: &[f64], y: impl Fn(usize) -> f64, from: usize) -> PlotPoints<'static> {
    let n = t.len();
    let step = ((n - from.min(n)) / 15000).max(1);
    PlotPoints::from(
        (from.min(n)..n)
            .step_by(step)
            .map(|i| [t[i], y(i)])
            .collect::<Vec<_>>(),
    )
}

fn phase_names(ex: &exercises::Exercise) -> (&'static str, &'static str) {
    match ex.out_is_concentric {
        Some(_) => ("Lift", "Lower"),
        None => ("Out", "Back"),
    }
}

// ---------- graphs ----------

/// Every graph in the app. The mouse wheel is handled by [`wheel_zoom`] instead of the default
/// (which pans the graph and also scrolls the page).
fn graph<'a>(id: &str) -> Plot<'a> {
    Plot::new(id).allow_scroll(false).allow_zoom(false)
}

/// Call first inside every graph's `show`: over a graph, the mouse wheel zooms the time axis
/// around the cursor (Shift + wheel zooms the vertical axis), and the page doesn't scroll.
/// Drag to pan, double-click to reset.
fn wheel_zoom(p: &mut egui_plot::PlotUi) {
    if !p.response().hovered() {
        return;
    }
    let (delta, shift) = p
        .ctx()
        .input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
    let amount = if delta.y != 0.0 { delta.y } else { delta.x };
    if amount != 0.0 {
        let factor = (amount * 0.003).exp();
        let zoom = if shift {
            egui::vec2(1.0, factor)
        } else {
            egui::vec2(factor, 1.0)
        };
        p.zoom_bounds_around_hovered(zoom);
    }
    // the graph used the wheel: stop the surrounding page from scrolling too
    p.ctx()
        .input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
}

// ---------- shared views ----------
fn summary_cards(ui: &mut egui::Ui, a: &Analysis, ex: &exercises::Exercise) {
    let s = &a.summary;
    let u = a.unit;
    let (lift, lower) = phase_names(ex);
    ui.horizontal_wrapped(|ui| {
        card(
            ui,
            "Reps",
            s.reps.to_string(),
            Some(ex.rep_note.to_string()),
            None,
        );
        card(
            ui,
            "Sets",
            s.sets.to_string(),
            s.rest_mean_s.map(|r| format!("avg rest {}", fmt_dur(r))),
            None,
        );
        card(
            ui,
            "Session time",
            fmt_dur(s.session_s),
            Some(format!("active {}", fmt_dur(s.active_s))),
            None,
        );
        card(
            ui,
            "Time under tension",
            format!("{:.0} s", s.tut_s),
            None,
            None,
        );
        card(
            ui,
            "Avg range of motion",
            format!("{:.0} {u}", s.rom_mean),
            Some(a.mode.label().to_string()),
            None,
        );
        let phases = match (s.concentric_mean, s.eccentric_mean) {
            (Some(c), Some(e)) => format!("{lift} {c:.1}s / {lower} {e:.1}s"),
            _ => format!("hold {:.1}s", s.hold_mean),
        };
        card(
            ui,
            "Avg rep time",
            format!("{:.1} s", s.duration_mean),
            Some(phases),
            None,
        );
        card(
            ui,
            "Avg peak speed",
            format!("{:.0} {u}/s", s.peak_speed_mean),
            None,
            None,
        );
    });
    ui.horizontal_wrapped(|ui| {
        score_card(ui, "QUALITY", s.scores.overall, "weighted overall");
        score_card(ui, "Consistency", s.scores.consistency, "same ROM and pace");
        score_card(ui, "Form", s.scores.form, "rep shape match");
        score_card(
            ui,
            "Tempo",
            s.scores.tempo,
            &format!("target {:.0}–{:.0} s", ex.target_rep_s.0, ex.target_rep_s.1),
        );
    });
    if !s.notes.is_empty() {
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Coaching notes").strong());
            for n in &s.notes {
                ui.label(format!("• {n}"));
            }
        });
    }
}

fn movement_plot(ui: &mut egui::Ui, id: &str, a: &Analysis, from_t: Option<f64>, height: f32) {
    let from = from_t
        .map(|t0| a.t.partition_point(|t| *t < t0))
        .unwrap_or(0);
    let min_t = from_t.unwrap_or(f64::MIN);
    let sig = &a.signal;
    graph(id)
        .height(height)
        .legend(Legend::default())
        .link_axis(egui::Id::unique(LINK), [true, false])
        .link_cursor(egui::Id::unique(LINK), [true, false])
        .x_axis_label("time (s)")
        .y_axis_label(format!("{} ({})", a.mode.label(), a.unit))
        .show(ui, |p| {
            wheel_zoom(p);
            for r in a.reps.iter().filter(|r| r.end_t >= min_t) {
                let c = if r.set % 2 == 0 {
                    Color32::from_rgba_unmultiplied(90, 140, 255, 28)
                } else {
                    Color32::from_rgba_unmultiplied(255, 160, 60, 28)
                };
                p.span(
                    Span::new("reps", r.start_t..=r.end_t)
                        .fill(c)
                        .border_width(0.0),
                );
            }
            if !sig.is_empty() {
                p.line(Line::new("movement", decimate(&a.t, |i| sig[i], from)).width(1.8));
                let pk: Vec<[f64; 2]> = a
                    .reps
                    .iter()
                    .filter(|r| r.end_t >= min_t)
                    .map(|r| [r.peak_t, sig[r.i_peak]])
                    .collect();
                p.points(
                    Points::new("rep peak", pk)
                        .radius(4.0)
                        .shape(MarkerShape::Circle)
                        .filled(true),
                );
                for r in a.reps.iter().filter(|r| r.end_t >= min_t) {
                    p.text(Text::new(
                        "rep #",
                        PlotPoint::new(r.peak_t, sig[r.i_peak]),
                        RichText::new(format!("  {}", r.index)).small(),
                    ));
                }
            }
        });
}

impl eframe::App for BioApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        ui.ctx().request_repaint_after(Duration::from_secs(1));

        if let Some(err) = self.setup_error.clone() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("BioLink could not start");
                ui.add_space(8.0);
                ui.label(RichText::new(err).color(RED));
                ui.add_space(8.0);
                ui.label("biolink.toml (next to biolink.exe, or in the folder you run it from) needs:");
                ui.code(
                    "database_url = \"https://biolinkdb-default-rtdb.firebaseio.com\"\n\
                     api_key = \"<your Web API key>\"\n\
                     postgres_url = \"postgresql://postgres:<password>@localhost:5432/biolink\"",
                );
                ui.label("PostgreSQL must be installed and running. The database and tables are created automatically.");
                ui.label("If the phone doesn't write to /current, set app_state_path to the node it does write to.");
            });
            return;
        }

        self.top_bar(ui);
        egui::Panel::left("sessions")
            .resizable(true)
            .default_size(290.0)
            .show(ui, |ui| self.session_list(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Session,
                    RichText::new("Exercise analysis").size(16.0),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Raw,
                    RichText::new("Raw data").size(16.0),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Progress,
                    RichText::new("Progress").size(16.0),
                );
                let live_label = if self.recording() { "Live ●" } else { "Live" };
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Live,
                    RichText::new(live_label).size(16.0),
                );
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.tab {
                    Tab::Session => self.session_view(ui),
                    Tab::Raw => self.raw_view(ui),
                    Tab::Progress => self.progress_view(ui),
                    Tab::Live => self.live_view(ui),
                });
        });
    }
}

impl BioApp {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("BioLink");
                ui.label(RichText::new("wrist exercise tracker").weak());
                ui.separator();
                let online = self.sensor_online();
                if online {
                    ui.label(RichText::new("● Sensor online").color(GREEN));
                } else {
                    ui.label(RichText::new("○ Sensor offline").weak());
                }
                ui.separator();
                match self.app_exercise() {
                    Some(ex) => {
                        ui.label(
                            RichText::new(format!("Phone: {}", exercises::name_of(&ex))).color(RED),
                        );
                    }
                    None => {
                        ui.label(RichText::new("Phone: no exercise").weak());
                    }
                }
                if self.recording() {
                    ui.separator();
                    ui.label(RichText::new("● saving to PostgreSQL").color(RED));
                    if ui.small_button("View live").clicked() {
                        self.tab = Tab::Live;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⟳ Refresh").clicked() {
                        self.send(Command::Refresh);
                        if let Some(id) = self.selected {
                            self.select(id);
                        }
                    }
                    ui.label(RichText::new(&self.status).small().weak());
                    if let Some((e, at)) = &self.error {
                        if at.elapsed() < Duration::from_secs(15) {
                            ui.label(RichText::new(e).small().color(RED));
                        }
                    }
                });
            });
        });
    }

    fn session_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("History");
        egui::ComboBox::from_id_salt("cat")
            .selected_text(self.filter_cat.unwrap_or("All categories"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter_cat, None, "All categories");
                for c in exercises::CATEGORIES {
                    ui.selectable_value(&mut self.filter_cat, Some(*c), *c);
                }
            });
        let mut present: Vec<String> = self.sessions.iter().map(|s| s.exercise.clone()).collect();
        present.sort();
        present.dedup();
        present.retain(|e| {
            self.filter_cat.is_none() || exercises::find(e).map(|x| x.category) == self.filter_cat
        });
        egui::ComboBox::from_id_salt("ex")
            .selected_text(
                self.filter_ex
                    .as_deref()
                    .map(exercises::name_of)
                    .unwrap_or("All exercises".into()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter_ex, None, "All exercises");
                for e in &present {
                    ui.selectable_value(
                        &mut self.filter_ex,
                        Some(e.clone()),
                        exercises::name_of(e),
                    );
                }
            });
        ui.separator();
        let mut clicked: Option<i64> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut shown = 0;
                for s in &self.sessions {
                    let cat = exercises::find(&s.exercise).map(|e| e.category);
                    if self.filter_cat.is_some() && cat != self.filter_cat {
                        continue;
                    }
                    if self.filter_ex.as_ref().is_some_and(|f| *f != s.exercise) {
                        continue;
                    }
                    shown += 1;
                    let detail = if s.in_progress() {
                        "● running".to_string()
                    } else if let Some(sum) = self.summaries.get(&s.id) {
                        format!(
                            "{} reps · {} · Q {}",
                            sum.reps,
                            fmt_dur(s.duration_s()),
                            fmt_opt(sum.scores.overall, |q| format!("{q:.0}"))
                        )
                    } else {
                        format!("{} · analysing…", fmt_dur(s.duration_s()))
                    };
                    let text = format!(
                        "{}\n{}   {}",
                        fmt_date(s.started_ms),
                        exercises::name_of(&s.exercise),
                        detail
                    );
                    let sel = self.selected == Some(s.id);
                    if ui
                        .add_sized(
                            [ui.available_width(), 36.0],
                            egui::Button::selectable(sel, text),
                        )
                        .clicked()
                    {
                        clicked = Some(s.id);
                    }
                }
                if shown == 0 {
                    ui.label(
                        RichText::new(
                            "No sessions yet.\nPress start in the mobile app\nwith the sensor on.",
                        )
                        .weak(),
                    );
                }
            });
        if let Some(id) = clicked {
            self.select(id);
            if self.tab != Tab::Raw {
                self.tab = Tab::Session;
            }
        }
    }

    /// Start/stop presses and elapsed time of a session.
    fn session_times(ui: &mut egui::Ui, info: &SessionInfo) {
        let clock = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default()
        };
        ui.label(fmt_date(info.started_ms));
        ui.separator();
        match info.stopped_ms {
            Some(stop) => ui.label(format!(
                "start {} · stop {} · elapsed {}",
                clock(info.started_ms),
                clock(stop),
                fmt_dur(info.duration_s())
            )),
            None => ui.label(
                RichText::new(format!(
                    "start {} · ● running {}",
                    clock(info.started_ms),
                    fmt_dur((now_ms() - info.started_ms as f64) / 1000.0)
                ))
                .color(RED),
            ),
        };
    }

    fn session_view(&mut self, ui: &mut egui::Ui) {
        let Some(l) = &self.loaded else {
            ui.label(if self.loading {
                "Loading session…"
            } else {
                "Select a session on the left."
            });
            return;
        };
        let info = &l.info;
        let ex = exercises::find_or_generic(&info.exercise);
        ui.horizontal(|ui| {
            ui.heading(exercises::name_of(&info.exercise));
            ui.label(RichText::new(ex.category).weak());
            ui.separator();
            Self::session_times(ui, info);
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Tracked by: {}", l.analysis.mode.label())).weak().small());
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "{:.0} Hz · ±{:.0} g · ±{:.0}°/s · {} samples",
                    l.series.hz,
                    32768.0 / info.accel_lsb_per_g,
                    32768.0 / info.gyro_lsb_per_dps,
                    info.samples
                ))
                .weak()
                .small(),
            );
            if l.series.gyro_bias.is_none() {
                ui.label(
                    RichText::new("no still moment found for gyro calibration")
                        .small()
                        .color(AMBER),
                );
            }
        });
        let a = &l.analysis;
        summary_cards(ui, a, ex);

        ui.add_space(6.0);
        ui.label(RichText::new("Movement (shaded = detected reps)").strong());
        movement_plot(ui, "sig", a, None, 240.0);

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Per rep").strong());
            for rm in RepMetric::ALL {
                ui.selectable_value(&mut self.rep_metric, rm, rm.label());
            }
        });
        rep_chart(ui, a, self.rep_metric, ex);

        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new("Sets").strong())
            .default_open(true)
            .show(ui, |ui| sets_table(ui, a));
        egui::CollapsingHeader::new(RichText::new("Reps").strong())
            .default_open(false)
            .show(ui, |ui| reps_table(ui, a, ex));
    }

    fn progress_view(&mut self, ui: &mut egui::Ui) {
        let mut exs: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| !s.in_progress())
            .map(|s| s.exercise.clone())
            .collect();
        exs.sort();
        exs.dedup();
        if exs.is_empty() {
            ui.label("No finished sessions yet.");
            return;
        }
        if self.progress_ex.as_ref().is_none_or(|e| !exs.contains(e)) {
            self.progress_ex = self
                .loaded
                .as_ref()
                .map(|l| l.info.exercise.clone())
                .filter(|e| exs.contains(e))
                .or(Some(exs[0].clone()));
        }
        let cur = self.progress_ex.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.heading("Progress for");
            egui::ComboBox::from_id_salt("pex")
                .selected_text(exercises::name_of(&cur))
                .show_ui(ui, |ui| {
                    for e in &exs {
                        ui.selectable_value(
                            &mut self.progress_ex,
                            Some(e.clone()),
                            exercises::name_of(e),
                        );
                    }
                });
        });
        let mut rows: Vec<(&SessionInfo, Option<&Summary>)> = self
            .sessions
            .iter()
            .filter(|s| !s.in_progress() && s.exercise == cur)
            .map(|s| (s, self.summaries.get(&s.id)))
            .collect();
        rows.sort_by_key(|r| r.0.started_ms);
        let pending = rows.iter().filter(|r| r.1.is_none()).count();
        if pending > 0 {
            ui.label(RichText::new(format!("Analysing {pending} session(s)…")).weak());
        }
        let done: Vec<(f64, &Summary)> = rows
            .iter()
            .filter_map(|r| r.1.map(|s| (r.0.started_ms as f64 / 86_400_000.0, s)))
            .collect();
        // ROM and speed are only comparable between sessions tracked with the same unit
        let unit = {
            let mut units: Vec<&str> = done.iter().filter(|d| d.1.reps > 0).map(|d| d.1.unit).collect();
            units.sort_unstable();
            let mut best = (exercises::find_or_generic(&cur).mode.unit(), 0);
            for u in &units {
                let c = units.iter().filter(|x| *x == u).count();
                if c > best.1 {
                    best = (u, c);
                }
            }
            best.0
        };

        let series_plot =
            |ui: &mut egui::Ui, id: &str, title: &str, lines: Vec<(&str, Vec<[f64; 2]>)>| {
                ui.label(RichText::new(title).strong());
                graph(id)
                    .height(170.0)
                    .legend(Legend::default())
                    .link_axis(egui::Id::unique("progress_x"), [true, false])
                    .x_axis_formatter(|mark, _| fmt_short_date(mark.value * 86_400_000.0))
                    .label_formatter(|h| match h {
                        egui_plot::HoverPosition::NearDataPoint {
                            plot_name,
                            position,
                            ..
                        } => Some(format!(
                            "{plot_name}\n{}\n{:.1}",
                            fmt_short_date(position.x * 86_400_000.0),
                            position.y
                        )),
                        egui_plot::HoverPosition::Elsewhere { .. } => None,
                    })
                    .show(ui, |p| {
                        wheel_zoom(p);
                        for (name, pts) in lines {
                            p.line(Line::new(name, PlotPoints::from(pts.clone())).width(1.5));
                            p.points(Points::new(name, pts).radius(3.5).filled(true));
                        }
                    });
            };
        let col = |f: &dyn Fn(&Summary) -> Option<f64>| -> Vec<[f64; 2]> {
            done.iter()
                .filter_map(|(x, s)| f(s).map(|v| [*x, v]))
                .collect()
        };

        ui.columns(2, |c| {
            series_plot(
                &mut c[0],
                "p_q",
                "Quality scores",
                vec![
                    ("Quality", col(&|s| s.scores.overall)),
                    ("Consistency", col(&|s| s.scores.consistency)),
                    ("Form", col(&|s| s.scores.form)),
                    ("Tempo", col(&|s| s.scores.tempo)),
                ],
            );
            series_plot(
                &mut c[1],
                "p_r",
                "Reps and sets",
                vec![
                    ("Reps", col(&|s| Some(s.reps as f64))),
                    ("Sets", col(&|s| Some(s.sets as f64))),
                ],
            );
            series_plot(
                &mut c[0],
                "p_rom",
                &format!("Avg range of motion ({unit})"),
                vec![("ROM", col(&|s| (s.reps > 0 && s.unit == unit).then_some(s.rom_mean)))],
            );
            series_plot(
                &mut c[1],
                "p_t",
                "Tempo (s)",
                vec![
                    (
                        "Rep time",
                        col(&|s| (s.reps > 0).then_some(s.duration_mean)),
                    ),
                    ("Lift", col(&|s| s.concentric_mean)),
                    ("Lower", col(&|s| s.eccentric_mean)),
                ],
            );
            series_plot(
                &mut c[0],
                "p_tut",
                "Time under tension (s)",
                vec![("TUT", col(&|s| Some(s.tut_s)))],
            );
            series_plot(
                &mut c[1],
                "p_speed",
                &format!("Avg peak speed ({unit}/s)"),
                vec![("Peak speed", col(&|s| (s.reps > 0 && s.unit == unit).then_some(s.peak_speed_mean)))],
            );
        });

        ui.add_space(8.0);
        ui.label(RichText::new("Sessions").strong());
        let mut open: Option<i64> = None;
        let rom_h = "ROM";
        egui::Grid::new("prog_table")
            .striped(true)
            .num_columns(8)
            .show(ui, |ui| {
                for h in [
                    "Date",
                    "Duration",
                    "Reps",
                    "Sets",
                    rom_h,
                    "Rep time",
                    "Quality",
                    "",
                ] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for (info, s) in rows.iter().rev() {
                    ui.label(fmt_date(info.started_ms));
                    ui.label(fmt_dur(info.duration_s()));
                    match s {
                        Some(s) => {
                            ui.label(s.reps.to_string());
                            ui.label(s.sets.to_string());
                            ui.label(format!("{:.0} {}", s.rom_mean, s.unit));
                            ui.label(format!("{:.1} s", s.duration_mean));
                            ui.label(fmt_opt(s.scores.overall, |q| format!("{q:.0}")));
                        }
                        None => {
                            for _ in 0..5 {
                                ui.label("…");
                            }
                        }
                    }
                    if ui.small_button("Open").clicked() {
                        open = Some(info.id);
                    }
                    ui.end_row();
                }
            });
        if let Some(id) = open {
            self.select(id);
            self.tab = Tab::Session;
        }
    }

    fn live_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(330.0);
                ui.label(RichText::new("Sensor (/test_imu)").strong());
                match self.live.imu_seen_ms {
                    Some(t) => {
                        let age = (now_ms() - t as f64) / 1000.0;
                        if age < 5.0 {
                            ui.label(RichText::new("● Updating").color(GREEN));
                        } else {
                            ui.label(
                                RichText::new(format!("○ No update for {}", fmt_dur(age))).weak(),
                            );
                        }
                        let n = self.live.last_batch_len;
                        let txt = RichText::new(format!("Last batch: {n} samples"));
                        ui.label(if n == 0 { txt.color(AMBER) } else { txt });
                    }
                    None => {
                        ui.label(RichText::new("No update received since the app started.").weak());
                    }
                }
            });
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(330.0);
                ui.label(RichText::new("Mobile app (/current)").strong());
                let clock = |ms: i64| {
                    chrono::DateTime::from_timestamp_millis(ms)
                        .map(|d| {
                            d.with_timezone(&chrono::Local)
                                .format("%H:%M:%S")
                                .to_string()
                        })
                        .unwrap_or_default()
                };
                match (&self.live.open, &self.live.app) {
                    (Some(s), _) => {
                        ui.label(
                            RichText::new(format!("● {}", exercises::name_of(&s.exercise)))
                                .size(18.0)
                                .color(RED),
                        );
                        ui.label(format!(
                            "Start pressed {} · elapsed {}",
                            clock(s.started_ms),
                            fmt_dur((now_ms() - s.started_ms as f64) / 1000.0)
                        ));
                        ui.label(RichText::new("samples are being saved").weak());
                    }
                    (None, Some(a)) => {
                        let when = a
                            .timestamp_ms
                            .map(|t| format!(" at {}", clock(t)))
                            .unwrap_or_default();
                        ui.label(format!(
                            "Stopped{when}: nothing is saved until the next start."
                        ));
                    }
                    (None, None) => {
                        ui.label(RichText::new("Nothing received from /current yet.").weak());
                    }
                }
            });
        });

        let Some(l) = &self.live_view else {
            ui.add_space(10.0);
            ui.label("Live charts appear here as soon as sensor data for an exercise arrives.");
            return;
        };
        let a = &l.analysis;
        let ex = exercises::find_or_generic(&l.info.exercise);
        let active = self.live.open.as_ref().is_some_and(|s| s.id == l.info.id);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(a.summary.reps.to_string())
                    .size(56.0)
                    .strong(),
            );
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(format!("reps · {}", exercises::name_of(&l.info.exercise)))
                        .size(18.0),
                );
                ui.label(
                    RichText::new(if active {
                        format!(
                            "elapsed {}",
                            fmt_dur((now_ms() - l.info.started_ms as f64) / 1000.0)
                        )
                    } else {
                        "stopped".into()
                    })
                    .weak(),
                );
            });
            if let Some(r) = a.reps.last() {
                ui.separator();
                let (lift, lower) = phase_names(ex);
                let phase = match (r.concentric_s, r.eccentric_s) {
                    (Some(c), Some(e)) => format!("{lift} {c:.1}s · {lower} {e:.1}s"),
                    _ => format!("hold {:.1}s", r.hold_s),
                };
                ui.label(
                    RichText::new(format!(
                        "Last rep: {:.0} {} · {:.1} s · {phase}",
                        r.rom, a.unit, r.duration_s
                    ))
                    .size(16.0),
                );
            }
        });
        summary_cards(ui, a, ex);
        let t_end = l.series.t.last().copied().unwrap_or(0.0);
        ui.label(RichText::new("Last 30 seconds").strong());
        movement_plot(ui, "live_sig", a, Some(t_end - 30.0), 220.0);
    }
}

fn rep_chart(ui: &mut egui::Ui, a: &Analysis, metric: RepMetric, ex: &exercises::Exercise) {
    let bars = |f: &dyn Fn(&Rep) -> f64| -> Vec<Bar> {
        a.reps
            .iter()
            .map(|r| Bar::new(r.index as f64, f(r)).width(0.7))
            .collect()
    };
    let (lift, lower) = phase_names(ex);
    graph("rep_chart")
        .height(170.0)
        .legend(Legend::default())
        .x_axis_label("rep #")
        .show(ui, |p| {
            wheel_zoom(p);
            match metric {
                RepMetric::Phases => {
                    let out_is_lift = ex.out_is_concentric.unwrap_or(true);
                    let (first, last) = if out_is_lift {
                        (lift, lower)
                    } else {
                        (lower, lift)
                    };
                    let c1 = BarChart::new(first, bars(&|r| r.out_s)).color(BLUE);
                    let c2 = BarChart::new("hold", bars(&|r| r.hold_s))
                        .color(Color32::from_rgb(160, 160, 160))
                        .stack_on(&[&c1]);
                    let c3 = BarChart::new(last, bars(&|r| r.back_s))
                        .color(ORANGE)
                        .stack_on(&[&c1, &c2]);
                    p.bar_chart(c1);
                    p.bar_chart(c2);
                    p.bar_chart(c3);
                }
                m => {
                    let (name, f): (String, Box<dyn Fn(&Rep) -> f64>) = match m {
                        RepMetric::Rom => (format!("ROM ({})", a.unit), Box::new(|r| r.rom)),
                        RepMetric::Duration => ("rep time (s)".into(), Box::new(|r| r.duration_s)),
                        RepMetric::Similarity => {
                            ("similarity (%)".into(), Box::new(|r| r.similarity * 100.0))
                        }
                        RepMetric::Speed | RepMetric::Phases => (
                            format!("peak speed ({}/s)", a.unit),
                            Box::new(|r| r.peak_speed),
                        ),
                    };
                    p.bar_chart(BarChart::new(name, bars(&*f)).color(BLUE));
                }
            }
        });
}

fn sets_table(ui: &mut egui::Ui, a: &Analysis) {
    if a.sets.is_empty() {
        ui.label("No sets detected.");
        return;
    }
    let rom_h = format!("Avg ROM ({})", a.unit);
    egui::Grid::new("sets")
        .striped(true)
        .num_columns(9)
        .show(ui, |ui| {
            for h in [
                "Set",
                "Reps",
                "Rest before",
                rom_h.as_str(),
                "ROM variation",
                "Avg rep time",
                "Speed loss",
                "ROM drop",
                "TUT",
            ] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for s in &a.sets {
                ui.label(s.index.to_string());
                ui.label(s.reps.to_string());
                ui.label(fmt_opt(s.rest_before_s, fmt_dur));
                ui.label(format!("{:.0}", s.rom_mean));
                ui.label(format!("{:.0}%", s.rom_cv * 100.0));
                ui.label(format!("{:.1} s", s.duration_mean));
                ui.label(fmt_opt(s.speed_loss_pct, |v| format!("{v:.0}%")));
                ui.label(fmt_opt(s.rom_drop_pct, |v| format!("{v:.0}%")));
                ui.label(format!("{:.0} s", s.tut_s));
                ui.end_row();
            }
        });
}

fn reps_table(ui: &mut egui::Ui, a: &Analysis, ex: &exercises::Exercise) {
    let (lift, lower) = phase_names(ex);
    let directional = ex.out_is_concentric.is_none();
    let rom_h = format!("ROM ({})", a.unit);
    egui::Grid::new("reps")
        .striped(true)
        .num_columns(10)
        .show(ui, |ui| {
            for h in [
                "#",
                "Set",
                "At",
                rom_h.as_str(),
                "Time",
                lift,
                lower,
                "Hold",
                "Peak speed",
                "Similarity",
            ] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for r in &a.reps {
                ui.label(r.index.to_string());
                ui.label((r.set + 1).to_string());
                ui.label(fmt_dur(r.start_t));
                ui.label(format!("{:.0}", r.rom));
                ui.label(format!("{:.1} s", r.duration_s));
                ui.label(fmt_opt(
                    r.concentric_s.or(directional.then_some(r.out_s)),
                    |v| format!("{v:.1} s"),
                ));
                ui.label(fmt_opt(
                    r.eccentric_s.or(directional.then_some(r.back_s)),
                    |v| format!("{v:.1} s"),
                ));
                ui.label(format!("{:.1} s", r.hold_s));
                ui.label(format!("{:.0}", r.peak_speed));
                let sim_score = ((r.similarity - 0.6) / 0.4 * 100.0).clamp(0.0, 100.0);
                ui.label(
                    RichText::new(format!("{:.0}%", r.similarity * 100.0))
                        .color(score_color(sim_score)),
                );
                ui.end_row();
            }
        });
}

// ---------- Raw data tab (independent of the exercise) ----------

type SeriesFn<'a> = (&'a str, Color32, &'a dyn Fn(usize) -> f64);

fn explain(ui: &mut egui::Ui, title: &str, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    ui.label(RichText::new(text).weak().small());
}

fn raw_lines(
    ui: &mut egui::Ui,
    id: &str,
    y_label: &str,
    t: &[f64],
    series: &[SeriesFn],
    hline: Option<f64>,
    height: f32,
) {
    graph(id)
        .height(height)
        .legend(Legend::default())
        .link_axis(egui::Id::unique("raw_x"), [true, false])
        .link_cursor(egui::Id::unique("raw_x"), [true, false])
        .x_axis_label("time since start press (s)")
        .y_axis_label(y_label)
        .show(ui, |p| {
            wheel_zoom(p);
            if let Some(y) = hline {
                p.hline(HLine::new("", y).color(GREY).width(0.5));
            }
            for (name, color, f) in series {
                p.line(Line::new(*name, decimate(t, f, 0)).color(*color).width(1.0));
            }
        });
}

const AXES: [&str; 3] = ["X", "Y", "Z"];

impl BioApp {
    fn raw_view(&mut self, ui: &mut egui::Ui) {
        let Some(l) = self.loaded.as_ref() else {
            ui.label(if self.loading {
                "Loading session…"
            } else {
                "Select a session on the left."
            });
            return;
        };
        ui.horizontal(|ui| {
            ui.heading(format!(
                "Raw data · {}",
                exercises::name_of(&l.info.exercise)
            ));
            ui.separator();
            Self::session_times(ui, &l.info);
        });
        ui.label(
            RichText::new(
                "Everything on this page comes straight from the sensor values. It doesn't depend on the exercise selected; \
                 only unit conversion and simple integration are applied.",
            )
            .weak(),
        );
        let lsb = (l.info.accel_lsb_per_g, l.info.gyro_lsb_per_dps);
        let Some(v) = l.raw.as_ref() else {
            ui.label("Computing…");
            return;
        };
        if v.t.len() < 2 {
            ui.label("No samples stored for this session yet.");
            return;
        }

        // ---- summary ----
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            card(
                ui,
                "Recorded",
                fmt_dur(v.duration()),
                Some(format!("{} samples · {:.0} Hz", v.samples, v.hz)),
                None,
            );
            card(
                ui,
                "Moving",
                format!("{:.0}%", v.moving_pct),
                Some("rotation >15°/s or accel ≠ 1 g".into()),
                None,
            );
            card(
                ui,
                "Peak acceleration",
                format!("{:.2} g", v.peak_acc_g),
                Some(format!("sensor limit ±{:.0} g", 32768.0 / lsb.0)),
                None,
            );
            card(
                ui,
                "Peak rotation speed",
                format!("{:.0}°/s", v.peak_gyr_dps),
                Some(format!("sensor limit ±{:.0}°/s", 32768.0 / lsb.1)),
                None,
            );
            card(
                ui,
                "Total rotation",
                format!("{:.0}°", v.total_rotation_deg),
                Some(format!("= {:.1} full turns", v.total_rotation_deg / 360.0)),
                None,
            );
            card(
                ui,
                "Vertical range",
                format!("{:.0} cm", v.vertical_range_cm),
                Some("estimated, see below".into()),
                None,
            );
            let (rhythm, sub) = match (v.rhythm_hz, v.rhythm_cycles) {
                (Some(f), Some(c)) => (
                    format!("{f:.2} Hz"),
                    Some(format!("one cycle every {:.1} s · ≈{c:.0} cycles", 1.0 / f)),
                ),
                _ => ("–".into(), Some("no clear repetition".into())),
            };
            card(ui, "Movement rhythm", rhythm, sub, None);
            let off = v
                .gyro_offset
                .map(|o| format!("{:.1} / {:.1} / {:.1}°/s", o[0], o[1], o[2]))
                .unwrap_or("not found".into());
            card(
                ui,
                "Gyro offset (X/Y/Z)",
                off,
                Some(if v.gaps > 0 {
                    format!("{} gaps filled", v.gaps)
                } else {
                    "no gaps in data".into()
                }),
                None,
            );
        });

        // ---- statistics ----
        ui.add_space(6.0);
        ui.label(RichText::new("Statistics").strong());
        egui::Grid::new("raw_stats")
            .striped(true)
            .num_columns(7)
            .show(ui, |ui| {
                for h in ["Sensor", "Axis", "Mean", "Min", "Max", "Std dev", "RMS"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for (sensor, unit, stats, digits) in [
                    ("Accelerometer", "g", &v.acc_stats, 3usize),
                    ("Gyroscope", "°/s", &v.gyr_stats, 1usize),
                ] {
                    for s in stats.iter() {
                        ui.label(sensor);
                        ui.label(s.name);
                        for x in [s.mean, s.min, s.max, s.std, s.rms] {
                            ui.label(format!("{x:.digits$} {unit}"));
                        }
                        ui.end_row();
                    }
                }
            });

        let t = &v.t;
        let colors = [RED, GREEN, BLUE];

        explain(
            ui,
            "Acceleration (g)",
            "What the accelerometer measures on each axis. When the wrist is still, the magnitude is 1 g (gravity) and the \
             split between X/Y/Z shows how the board is tilted. Movement shows up as the magnitude moving away from 1 g.",
        );
        let ax = |k: usize| move |i: usize| v.acc[i][k];
        let (a0, a1, a2) = (ax(0), ax(1), ax(2));
        let am = |i: usize| v.acc_mag[i];
        raw_lines(
            ui,
            "raw_acc",
            "g",
            t,
            &[
                ("X", colors[0], &a0),
                ("Y", colors[1], &a1),
                ("Z", colors[2], &a2),
                ("magnitude", GREY, &am),
            ],
            Some(1.0),
            200.0,
        );

        explain(
            ui,
            "Rotation speed (°/s)",
            "How fast the wrist turns around each of the board's axes. Zero when it isn't rotating.",
        );
        let gx = |k: usize| move |i: usize| v.gyr[i][k];
        let (g0, g1, g2) = (gx(0), gx(1), gx(2));
        let gm = |i: usize| v.gyr_mag[i];
        raw_lines(
            ui,
            "raw_gyr",
            "°/s",
            t,
            &[
                ("X", colors[0], &g0),
                ("Y", colors[1], &g1),
                ("Z", colors[2], &g2),
                ("magnitude", GREY, &gm),
            ],
            Some(0.0),
            200.0,
        );

        explain(
            ui,
            "Tilt from gravity (°)",
            "The board's angle worked out from the direction of gravity alone. Accurate when the arm moves slowly; \
             it jumps around during fast moves, because the movement itself also pushes on the accelerometer.",
        );
        let (pitch, roll) = (|i: usize| v.pitch[i], |i: usize| v.roll[i]);
        raw_lines(
            ui,
            "raw_tilt",
            "°",
            t,
            &[("pitch", ORANGE, &pitch), ("roll", BLUE, &roll)],
            Some(0.0),
            180.0,
        );

        explain(
            ui,
            "Integrated rotation (°)",
            "Rotation speed added up over time: how far the wrist has turned around each axis since the start press. \
             A small error in the gyro makes this slowly drift, so look at the swings rather than the absolute level.",
        );
        let rx = |k: usize| move |i: usize| v.angle[i][k];
        let (r0, r1, r2) = (rx(0), rx(1), rx(2));
        raw_lines(
            ui,
            "raw_angle",
            "°",
            t,
            &[
                ("X", colors[0], &r0),
                ("Y", colors[1], &r1),
                ("Z", colors[2], &r2),
            ],
            Some(0.0),
            180.0,
        );

        explain(
            ui,
            "Vertical motion",
            "Up/down movement of the wrist with gravity removed (gravity direction tracked with gyro + accelerometer), \
             then integrated once for speed and twice for position. Integration amplifies small errors: positions are \
             estimates, and without drift removal they run away within seconds.",
        );
        let (ua, uh) = (|i: usize| v.up_acc[i], |i: usize| v.horiz_acc[i]);
        raw_lines(
            ui,
            "raw_upacc",
            "m/s²",
            t,
            &[
                ("up/down acceleration", ORANGE, &ua),
                ("sideways acceleration (size)", GREY, &uh),
            ],
            Some(0.0),
            160.0,
        );
        let uv = |i: usize| v.up_vel[i];
        raw_lines(
            ui,
            "raw_upvel",
            "m/s",
            t,
            &[("up/down speed", BLUE, &uv)],
            Some(0.0),
            140.0,
        );
        let up = |i: usize| v.up_pos_cm[i];
        raw_lines(
            ui,
            "raw_uppos",
            "cm",
            t,
            &[("height change", GREEN, &up)],
            Some(0.0),
            160.0,
        );

        explain(
            ui,
            "Frequency content",
            "How strongly the movement repeats at each rate, for the busiest axis of each sensor. A peak at 0.4 Hz means \
             something repeats every 2.5 s; walking or tremor show up as peaks at higher rates.",
        );
        let acc_name = format!("accel {}", AXES[v.acc_axis]);
        let gyr_name = format!("gyro {}", AXES[v.gyr_axis]);
        graph("raw_spec")
            .height(180.0)
            .legend(Legend::default())
            .x_axis_label("frequency (Hz)")
            .y_axis_label("relative strength")
            .show(ui, |p| {
                wheel_zoom(p);
                p.line(
                    Line::new(acc_name.as_str(), PlotPoints::from(v.acc_spectrum.clone()))
                        .color(ORANGE),
                );
                p.line(
                    Line::new(gyr_name.as_str(), PlotPoints::from(v.gyr_spectrum.clone()))
                        .color(BLUE),
                );
                if let Some(f) = v.rhythm_hz {
                    p.vline(
                        egui_plot::VLine::new("rhythm", f)
                            .color(GREY)
                            .style(egui_plot::LineStyle::dashed_dense()),
                    );
                }
            });
    }
}
