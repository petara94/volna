use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    Error, Result,
    model::{GridSpec, HISTORY_LIMIT, ObjectPose, Point, Scene},
    persistence,
    solver::{PreparedGeometry, Solver},
};

/// Высокий темп воспроизведения; пакет по-прежнему ограничен бюджетом 8 мс.
/// Это число шагов на кадр, а не изменение физической скорости или шага времени.
pub const DEFAULT_STEPS_PER_BATCH: u32 = 128;

pub enum Command {
    Apply {
        request_id: u64,
        scene: Scene,
    },
    Play(bool),
    Step,
    Reset,
    Speed(u32),
    Load {
        request_id: u64,
        path: PathBuf,
    },
    SetPose {
        epoch: u64,
        request_id: u64,
        pose: ObjectPose,
    },
    Save {
        path: PathBuf,
        scene: Scene,
        overwrite: bool,
    },
    Export {
        path: PathBuf,
        overwrite: bool,
    },
}

pub enum Event {
    Applied {
        epoch: u64,
        request_id: u64,
        revision: u64,
        scene: Box<Scene>,
    },
    Error {
        epoch: u64,
        request_id: u64,
        message: String,
    },
    PoseResult {
        epoch: u64,
        request_id: u64,
        object_id: u64,
        result: std::result::Result<u64, String>,
    },
    FileSaved {
        path: PathBuf,
    },
}

#[derive(Clone)]
pub struct ProbeTrace {
    pub id: u64,
    pub name: String,
    pub sample_position: Point,
    pub samples: Vec<[f64; 2]>,
    pub rms: Option<f64>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub revision: u64,
    pub scene: Arc<Scene>,
    pub epoch: u64,
    pub run: u64,
    pub inherited_fields: bool,
    pub history_start_step: u64,
    pub history_start_time_s: f64,
    pub grid: GridSpec,
    pub ez: Vec<f64>,
    pub rms: Option<Vec<f64>>,
    pub mask: Arc<Vec<bool>>,
    pub time_s: f64,
    pub step: u64,
    pub rms_progress: f64,
    pub probes: Vec<ProbeTrace>,
    pub steps_per_second: f64,
    pub frame_rate: f64,
    pub running: bool,
}

struct LiveRequest {
    epoch: u64,
    request_id: u64,
    pose: ObjectPose,
}

struct Control {
    command: Command,
    // Assigned only after a successful FIFO admission, while holding the gate.
    barrier_epoch: Option<u64>,
}

struct Admission {
    epoch: u64,
    barriers: usize,
    known_ids: HashSet<u64>,
    poses: HashMap<u64, LiveRequest>,
    order: VecDeque<u64>,
}

fn object_ids(scene: &Scene) -> HashSet<u64> {
    scene
        .sources
        .iter()
        .map(|s| s.id)
        .chain(scene.reflectors.iter().map(|r| r.id))
        .chain(scene.probes.iter().map(|p| p.id))
        .collect()
}

impl Admission {
    fn new(scene: Arc<Scene>) -> Self {
        Self {
            epoch: 1,
            barriers: 0,
            known_ids: object_ids(&scene),
            poses: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn enqueue_pose(&mut self, request: LiveRequest) -> Result<()> {
        if request.epoch != self.epoch || self.barriers != 0 {
            return Err(Error::Invalid(
                "жест отменён сменой сцены; дождитесь нового поколения".into(),
            ));
        }
        let id = request.pose.id();
        if !self.known_ids.contains(&id) {
            return Err(Error::Invalid(format!("неизвестный объект {id}")));
        }
        // Updating an existing ID keeps its turn; a hot ID cannot starve others.
        if self.poses.insert(id, request).is_none() {
            self.order.push_back(id);
        }
        Ok(())
    }

    fn take_pose(&mut self) -> Option<LiveRequest> {
        if self.barriers != 0 {
            return None;
        }
        let id = self.order.pop_front()?;
        self.poses.remove(&id)
    }
}

struct Shared {
    snapshot: Mutex<Option<Arc<Snapshot>>>,
    snapshot_ready: Condvar,
    events: Mutex<VecDeque<Event>>,
    events_ready: Condvar,
    gate: Mutex<Admission>,
    shutdown: AtomicBool,
}

impl Shared {
    fn new(scene: Arc<Scene>) -> Self {
        Self {
            snapshot: Mutex::new(None),
            snapshot_ready: Condvar::new(),
            events: Mutex::new(VecDeque::new()),
            events_ready: Condvar::new(),
            gate: Mutex::new(Admission::new(scene)),
            shutdown: AtomicBool::new(false),
        }
    }
}

pub struct Runtime {
    sender: mpsc::SyncSender<Control>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

fn poisoned() -> Error {
    Error::Numerical("нарушен внутренний инвариант синхронизации worker".into())
}

impl Runtime {
    pub fn new(scene: Scene) -> Result<Self> {
        let scene = Arc::new(scene);
        let shared = Arc::new(Shared::new(Arc::clone(&scene)));
        let (sender, receiver) = mpsc::sync_channel(32);
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("volna-solver".into())
            .spawn(move || {
                if let Err(error) = worker(scene, &worker_shared, &receiver) {
                    emit(
                        &worker_shared,
                        Event::Error {
                            epoch: 0,
                            request_id: 0,
                            message: error.to_string(),
                        },
                    );
                }
            })?;
        Ok(Self {
            sender,
            shared,
            thread: Some(thread),
        })
    }

    pub fn send(&self, command: Command) -> Result<()> {
        if self.thread.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(Error::Numerical(
                "вычислительный поток завершился; перезапустите приложение".into(),
            ));
        }
        admit(&self.shared, &self.sender, command)
    }

    pub fn poll(&self) -> Result<Option<Arc<Snapshot>>> {
        Ok(self.shared.snapshot.lock().map_err(|_| poisoned())?.take())
    }

    pub fn event(&self) -> Option<Event> {
        match self.shared.events.lock() {
            Ok(mut events) => events.pop_front(),
            Err(_) => Some(Event::Error {
                epoch: 0,
                request_id: 0,
                message: poisoned().to_string(),
            }),
        }
    }

    pub fn wait_event(&self, timeout: Duration) -> Result<Option<Event>> {
        let events = self.shared.events.lock().map_err(|_| poisoned())?;
        let (mut events, _) = self
            .shared
            .events_ready
            .wait_timeout_while(events, timeout, |q| q.is_empty())
            .map_err(|_| poisoned())?;
        Ok(events.pop_front())
    }

    pub fn wait_snapshot(&self, timeout: Duration) -> Result<Option<Arc<Snapshot>>> {
        let snapshot = self.shared.snapshot.lock().map_err(|_| poisoned())?;
        let (mut snapshot, _) = self
            .shared
            .snapshot_ready
            .wait_timeout_while(snapshot, timeout, |s| s.is_none())
            .map_err(|_| poisoned())?;
        Ok(snapshot.take())
    }
}

fn admit(shared: &Shared, sender: &mpsc::SyncSender<Control>, command: Command) -> Result<()> {
    let mut gate = shared.gate.lock().map_err(|_| poisoned())?;
    if let Command::SetPose {
        epoch,
        request_id,
        pose,
    } = command
    {
        return gate.enqueue_pose(LiveRequest {
            epoch,
            request_id,
            pose,
        });
    }
    let barrier_epoch = if matches!(
        command,
        Command::Apply { .. } | Command::Load { .. } | Command::Reset
    ) {
        Some(
            gate.epoch
                .checked_add(1)
                .ok_or_else(|| Error::Invalid("переполнение поколения сцены".into()))?,
        )
    } else {
        None
    };
    sender
        .try_send(Control {
            command,
            barrier_epoch,
        })
        .map_err(|error| match error {
            mpsc::TrySendError::Full(_) => {
                Error::Invalid("очередь управления заполнена; дождитесь выполнения команды".into())
            }
            mpsc::TrySendError::Disconnected(_) => {
                Error::Numerical("вычислительный поток недоступен".into())
            }
        })?;
    // No epoch change or lost gesture when try_send returned Full.
    if let Some(epoch) = barrier_epoch {
        gate.epoch = epoch;
        gate.barriers += 1;
        gate.poses.clear();
        gate.order.clear();
    }
    Ok(())
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            eprintln!("Volna: вычислительный поток завершился с panic");
        }
    }
}

fn emit(shared: &Shared, event: Event) {
    if let Ok(mut events) = shared.events.lock() {
        match &event {
            Event::Applied { epoch, .. }
            | Event::Error {
                epoch,
                request_id: 1..,
                ..
            } => {
                events.retain(|e| {
                    !matches!(
                        e,
                        Event::Applied { .. }
                            | Event::Error {
                                request_id: 1..,
                                ..
                            }
                    ) && !matches!(e, Event::PoseResult { epoch: old, .. } if old < epoch)
                });
            }
            Event::Error { request_id: 0, .. } => {
                events.retain(|e| !matches!(e, Event::Error { request_id: 0, .. }));
            }
            Event::PoseResult {
                epoch, object_id, ..
            } => {
                // One acknowledgement per accepted object in the current epoch.
                // Unknown IDs never enter the mailbox, and cold events retire old epochs.
                events.retain(|e| {
                    !matches!(e, Event::PoseResult { epoch: old, object_id: id, .. }
                    if old < epoch || (old == epoch && id == object_id))
                });
            }
            Event::FileSaved { .. } => {
                if events
                    .iter()
                    .filter(|e| matches!(e, Event::FileSaved { .. }))
                    .count()
                    >= 16
                    && let Some(i) = events
                        .iter()
                        .position(|e| matches!(e, Event::FileSaved { .. }))
                {
                    events.remove(i);
                }
            }
        }
        events.push_back(event);
        shared.events_ready.notify_all();
    } else {
        shared.shutdown.store(true, Ordering::Release);
        eprintln!("Volna: {}", poisoned());
    }
}

struct PreparedPose {
    scene: Arc<Scene>,
    geometry: Option<PreparedGeometry>,
    probe_indices: Vec<usize>,
}

struct State {
    scene: Arc<Scene>,
    solver: Solver,
    epoch: u64,
    revision: u64,
    run: u64,
    inherited_fields: bool,
    history_start_step: u64,
    history_start_time_s: f64,
    running: bool,
    failed: bool,
    speed: u32,
    history: VecDeque<(f64, Vec<f64>)>,
    probe_indices: Vec<usize>,
    mask: Arc<Vec<bool>>,
    last_good: Option<Arc<Snapshot>>,
    rate_start: Instant,
    rate_steps: u64,
    rate_frames: u64,
    last_frame_step: u64,
    steps_per_second: f64,
    frame_rate: f64,
}

fn probe_indices(scene: &Scene, grid: GridSpec) -> Vec<usize> {
    scene
        .probes
        .iter()
        .map(|p| {
            let (x, y) = grid.nearest(p.position);
            grid.index(x, y)
        })
        .collect()
}

impl State {
    fn new(scene: Arc<Scene>, epoch: u64, revision: u64) -> Result<Self> {
        let solver = Solver::new(&scene)?;
        let probe_indices = probe_indices(&scene, solver.grid());
        let mask = Arc::new(solver.mask().to_vec());
        Ok(Self {
            scene,
            solver,
            epoch,
            revision,
            run: 1,
            inherited_fields: false,
            history_start_step: 0,
            history_start_time_s: 0.0,
            running: true,
            failed: false,
            speed: DEFAULT_STEPS_PER_BATCH,
            history: VecDeque::new(),
            probe_indices,
            mask,
            last_good: None,
            rate_start: Instant::now(),
            rate_steps: 0,
            rate_frames: 0,
            last_frame_step: 0,
            steps_per_second: 0.0,
            frame_rate: 0.0,
        })
    }

    fn record(&mut self) {
        if self.probe_indices.is_empty() {
            return;
        }
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back((
            self.solver.time(),
            self.probe_indices
                .iter()
                .map(|&i| self.solver.ez()[i])
                .collect(),
        ));
    }

    fn runtime_error(&self, shared: &Shared, message: String) {
        emit(
            shared,
            Event::Error {
                epoch: self.epoch,
                request_id: 0,
                message,
            },
        );
    }

    fn advance(&mut self, shared: &Shared) -> bool {
        if self.failed {
            self.runtime_error(
                shared,
                "После численной ошибки требуется Reset или новая сцена".into(),
            );
            return false;
        }
        match self.solver.step() {
            Ok(()) => {
                self.record();
                true
            }
            Err(error) => {
                self.running = false;
                self.failed = true;
                self.runtime_error(shared, error.to_string());
                // Roll history back to the same safe frame used by snapshots/export.
                if let Some(last) = &self.last_good {
                    while self
                        .history
                        .back()
                        .is_some_and(|(time, _)| *time > last.time_s)
                    {
                        self.history.pop_back();
                    }
                }
                false
            }
        }
    }

    fn publish(&mut self, shared: &Shared) -> Result<()> {
        if self.failed {
            if let Some(last) = &self.last_good {
                let mut snapshot = (**last).clone();
                snapshot.epoch = self.epoch;
                snapshot.running = false;
                snapshot.steps_per_second = 0.0;
                snapshot.frame_rate = 0.0;
                let snapshot = Arc::new(snapshot);
                self.last_good = Some(Arc::clone(&snapshot));
                *shared.snapshot.lock().map_err(|_| poisoned())? = Some(snapshot);
                shared.snapshot_ready.notify_all();
            }
            return Ok(());
        }
        let grid = self.solver.grid();
        if self.solver.steps() != self.last_frame_step {
            self.rate_frames += 1;
            self.last_frame_step = self.solver.steps();
        }
        let elapsed = self.rate_start.elapsed().as_secs_f64();
        if elapsed >= 0.5 {
            self.steps_per_second =
                self.solver.steps().saturating_sub(self.rate_steps) as f64 / elapsed;
            self.frame_rate = self.rate_frames as f64 / elapsed;
            self.rate_steps = self.solver.steps();
            self.rate_frames = 0;
            self.rate_start = Instant::now();
        }
        let probes = self
            .scene
            .probes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let index = self.probe_indices[i];
                ProbeTrace {
                    id: p.id,
                    name: p.name.clone(),
                    sample_position: grid.point(index % grid.nx, index / grid.nx),
                    samples: plot_samples(&self.history, i),
                    rms: self.solver.rms().map(|r| r[index]),
                }
            })
            .collect();
        let snapshot = Arc::new(Snapshot {
            revision: self.revision,
            scene: Arc::clone(&self.scene),
            epoch: self.epoch,
            run: self.run,
            inherited_fields: self.inherited_fields,
            history_start_step: self.history_start_step,
            history_start_time_s: self.history_start_time_s,
            grid,
            ez: self.solver.ez().to_vec(),
            rms: self.solver.rms().map(<[f64]>::to_vec),
            mask: Arc::clone(&self.mask),
            time_s: self.solver.time(),
            step: self.solver.steps(),
            rms_progress: self.solver.rms_progress(),
            probes,
            steps_per_second: if self.running {
                self.steps_per_second
            } else {
                0.0
            },
            frame_rate: if self.running { self.frame_rate } else { 0.0 },
            running: self.running,
        });
        self.last_good = Some(Arc::clone(&snapshot));
        *shared.snapshot.lock().map_err(|_| poisoned())? = Some(snapshot);
        shared.snapshot_ready.notify_all();
        Ok(())
    }

    fn prepare_pose(&self, pose: &ObjectPose) -> Result<Option<PreparedPose>> {
        if self.failed {
            return Err(Error::Numerical(
                "После численной ошибки требуется Reset или новая сцена".into(),
            ));
        }
        if self.scene.pose(pose.id()).as_ref() == Some(pose) {
            return Ok(None);
        }
        // Only the accepted scene plus this ONE consumed absolute pose is a candidate.
        let candidate = self.scene.with_pose(pose)?;
        let geometry = if matches!(pose, ObjectPose::Probe { .. }) {
            None
        } else {
            Some(self.solver.prepare_geometry(&self.scene, &candidate)?)
        };
        Ok(Some(PreparedPose {
            probe_indices: probe_indices(&candidate, self.solver.grid()),
            scene: Arc::new(candidate),
            geometry,
        }))
    }

    fn commit_pose(
        &mut self,
        request: LiveRequest,
        prepared: Result<Option<PreparedPose>>,
        shared: &Shared,
    ) -> Result<()> {
        let mut changed = false;
        let mut geometry_changed = false;
        let result = {
            let gate = shared.gate.lock().map_err(|_| poisoned())?;
            // A newer same-epoch pose does NOT cancel this prepared frame.
            // Cold admission and this infallible commit have one total order.
            if gate.epoch != request.epoch || gate.barriers != 0 {
                return Ok(());
            }
            match prepared {
                Err(error) => Err(error.to_string()),
                Ok(None) => Ok(self.revision),
                Ok(Some(prepared)) => match self.revision.checked_add(1) {
                    None => Err("переполнение принятой версии сцены".into()),
                    Some(revision) => {
                        if let Some(geometry) = prepared.geometry {
                            self.solver.commit_geometry(geometry);
                            geometry_changed = true;
                        }
                        self.scene = prepared.scene;
                        self.probe_indices = prepared.probe_indices;
                        self.revision = revision;
                        changed = true;
                        Ok(revision)
                    }
                },
            }
        };
        // Large copies, destruction of history, and snapshots never hold the GUI gate.
        if changed {
            self.inherited_fields |= geometry_changed;
            self.history.clear();
            self.history_start_step = self.solver.steps();
            self.history_start_time_s = self.solver.time();
            if geometry_changed {
                self.mask = Arc::new(self.solver.mask().to_vec());
            }
        }
        if result.is_ok() {
            self.publish(shared)?;
        }
        emit(
            shared,
            Event::PoseResult {
                epoch: request.epoch,
                request_id: request.request_id,
                object_id: request.pose.id(),
                result,
            },
        );
        Ok(())
    }

    fn replace(
        &mut self,
        scene: Result<Scene>,
        epoch: u64,
        request_id: u64,
        shared: &Shared,
    ) -> Result<()> {
        let prepared = scene.and_then(|scene| {
            let revision = self
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::Invalid("переполнение принятой версии сцены".into()))?;
            let run = self
                .run
                .checked_add(1)
                .ok_or_else(|| Error::Invalid("переполнение номера запуска".into()))?;
            let mut next = State::new(Arc::new(scene), epoch, revision)?;
            next.run = run;
            next.running = self.running;
            next.speed = self.speed;
            Ok(next)
        });
        self.epoch = epoch;
        let event = match prepared {
            Ok(next) => {
                let known_ids = object_ids(&next.scene);
                let old = {
                    let mut gate = shared.gate.lock().map_err(|_| poisoned())?;
                    gate.known_ids = known_ids;
                    gate.barriers -= 1;
                    std::mem::replace(self, next)
                };
                drop(old);
                Event::Applied {
                    epoch,
                    request_id,
                    revision: self.revision,
                    scene: Box::new((*self.scene).clone()),
                }
            }
            Err(error) => {
                shared.gate.lock().map_err(|_| poisoned())?.barriers -= 1;
                Event::Error {
                    epoch,
                    request_id,
                    message: error.to_string(),
                }
            }
        };
        // Even a rejected barrier cancels the old gesture and publishes its new epoch.
        self.publish(shared)?;
        emit(shared, event);
        Ok(())
    }

    fn metadata(&self) -> Result<serde_json::Value> {
        let (step, time) = if self.failed {
            self.last_good
                .as_ref()
                .map(|s| (s.step, s.time_s))
                .unwrap_or((0, 0.0))
        } else {
            (self.solver.steps(), self.solver.time())
        };
        Ok(serde_json::json!({
            "schema_version": 1, "solver": "volna-yee-tmz-cpml-f64", "version": env!("CARGO_PKG_VERSION"),
            "scene": self.scene.as_ref(), "scene_sha256": persistence::scene_hash(&self.scene)?,
            "epoch": self.epoch, "revision": self.revision, "run": self.run, "step": step,
            "inherited_fields": self.inherited_fields, "history_start_step": self.history_start_step,
            "history_start_time_s": self.history_start_time_s,
            "time_s": time, "dt_s": self.solver.grid().dt, "solver_threads": 1,
            "rustc": env!("VOLNA_BUILD_RUSTC"), "target": env!("VOLNA_BUILD_TARGET"),
            "profile": env!("VOLNA_BUILD_PROFILE"), "git_revision": env!("VOLNA_BUILD_REVISION"),
            "git_dirty": env!("VOLNA_BUILD_DIRTY"), "os": std::env::consts::OS,
            "history_limit": HISTORY_LIMIT, "spectrum": "user_declared_fmax_not_verified",
            "units": {"time": "s", "electric_field": "V/m", "position": "m"}
        }))
    }

    fn command(&mut self, control: Control, shared: &Shared) -> Result<()> {
        let epoch = control.barrier_epoch;
        match control.command {
            Command::Apply { request_id, scene } => {
                self.replace(Ok(scene), epoch.ok_or_else(poisoned)?, request_id, shared)?
            }
            Command::Load { request_id, path } => self.replace(
                persistence::load_scene(&path),
                epoch.ok_or_else(poisoned)?,
                request_id,
                shared,
            )?,
            Command::Reset => self.replace(
                Ok((*self.scene).clone()),
                epoch.ok_or_else(poisoned)?,
                0,
                shared,
            )?,
            Command::Play(running) => {
                if running && self.failed {
                    self.runtime_error(
                        shared,
                        "После численной ошибки требуется Reset или новая сцена".into(),
                    );
                } else {
                    self.running = running;
                    self.publish(shared)?;
                }
            }
            Command::Step => {
                self.running = false;
                self.advance(shared);
                self.publish(shared)?;
            }
            Command::Speed(speed) => {
                if (1..=128).contains(&speed) {
                    self.speed = speed;
                } else {
                    self.runtime_error(shared, "Скорость: от 1 до 128 шагов на кадр".into());
                }
            }
            Command::Save {
                path,
                scene,
                overwrite,
            } => match persistence::save_scene(&path, &scene, overwrite) {
                Ok(()) => emit(shared, Event::FileSaved { path }),
                Err(error) => self.runtime_error(shared, error.to_string()),
            },
            Command::Export { path, overwrite } => {
                let names: Vec<_> = self.scene.probes.iter().map(|p| p.name.clone()).collect();
                let samples: Vec<_> = self.history.iter().cloned().collect();
                let result = self.metadata().and_then(|metadata| {
                    persistence::export_probes_with_metadata(
                        &path, &names, &samples, overwrite, &metadata,
                    )
                });
                match result {
                    Ok(()) => emit(shared, Event::FileSaved { path }),
                    Err(error) => self.runtime_error(shared, error.to_string()),
                }
            }
            Command::SetPose { .. } => return Err(poisoned()), // Never admitted to the control FIFO.
        }
        Ok(())
    }
}

fn worker(scene: Arc<Scene>, shared: &Shared, receiver: &mpsc::Receiver<Control>) -> Result<()> {
    let mut state = State::new(scene, 1, 1)?;
    state.publish(shared)?;
    emit(
        shared,
        Event::Applied {
            epoch: 1,
            request_id: 1,
            revision: 1,
            scene: Box::new((*state.scene).clone()),
        },
    );
    let frame_period = Duration::from_micros(16_667);
    let live_period = Duration::from_micros(33_333);
    let mut next_frame = Instant::now() + frame_period;
    let mut next_live = Instant::now();
    while !shared.shutdown.load(Ordering::Acquire) {
        for _ in 0..32 {
            match receiver.try_recv() {
                Ok(command) => state.command(command, shared)?,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if Instant::now() >= next_live {
            let request = shared.gate.lock().map_err(|_| poisoned())?.take_pose();
            if let Some(request) = request {
                let start = Instant::now();
                let prepared = state.prepare_pose(&request.pose);
                state.commit_pose(request, prepared, shared)?;
                next_live = start + live_period;
            }
        }
        // Never drain the live mailbox before allowing a physical batch/frame.
        if state.running && Instant::now() >= next_frame {
            let start = Instant::now();
            for _ in 0..state.speed {
                if shared.shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }
                if !state.advance(shared) || start.elapsed() >= Duration::from_millis(8) {
                    break;
                }
            }
            state.publish(shared)?;
            next_frame = Instant::now() + frame_period.saturating_sub(start.elapsed());
        }
        let wait = if state.running {
            next_frame
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5))
        } else {
            Duration::from_millis(5)
        };
        match receiver.recv_timeout(wait) {
            Ok(command) => state.command(command, shared)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
    Ok(())
}

fn plot_samples(history: &VecDeque<(f64, Vec<f64>)>, probe: usize) -> Vec<[f64; 2]> {
    if history.len() <= 4096 {
        return history
            .iter()
            .map(|(t, values)| [*t, values[probe]])
            .collect();
    }
    // Min/max-огибающая только для отрисовки. CSV сохраняет непрореженные отсчёты.
    let bucket = history.len().div_ceil(2048);
    let mut output = Vec::with_capacity(4096);
    for start in (0..history.len()).step_by(bucket) {
        let end = (start + bucket).min(history.len());
        let mut lo = start;
        let mut hi = start;
        for i in start + 1..end {
            if history[i].1[probe] < history[lo].1[probe] {
                lo = i;
            }
            if history[i].1[probe] > history[hi].1[probe] {
                hi = i;
            }
        }
        for i in [lo.min(hi), lo.max(hi)] {
            output.push([history[i].0, history[i].1[probe]]);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        State,
        Shared,
        mpsc::SyncSender<Control>,
        mpsc::Receiver<Control>,
    ) {
        let mut scene = Scene::default();
        scene.sources.clear();
        scene.domain.width_m = 0.04;
        scene.domain.height_m = 0.03;
        scene.domain.pml_cells = 8;
        scene.probes = vec![
            crate::model::Probe {
                id: 1,
                name: "a".into(),
                position: Point { x: 0.01, y: 0.01 },
            },
            crate::model::Probe {
                id: 2,
                name: "b".into(),
                position: Point { x: 0.02, y: 0.01 },
            },
        ];
        let scene = Arc::new(scene);
        let shared = Shared::new(Arc::clone(&scene));
        let mut state = State::new(scene, 1, 1).unwrap();
        state.running = false;
        state.publish(&shared).unwrap();
        let (sender, receiver) = mpsc::sync_channel(32);
        (state, shared, sender, receiver)
    }

    fn request(id: u64, request_id: u64, x: f64) -> LiveRequest {
        LiveRequest {
            epoch: 1,
            request_id,
            pose: ObjectPose::Probe {
                id,
                position: Point { x, y: 0.01 },
            },
        }
    }

    #[test]
    fn high_default_batch_preserves_physics_and_explicit_speed_across_reset() {
        let (mut state, shared, sender, receiver) = fixture();
        assert_eq!(DEFAULT_STEPS_PER_BATCH, 128);
        assert_eq!(state.speed, DEFAULT_STEPS_PER_BATCH);
        let grid = state.solver.grid();
        for speed in [1, 37, 128] {
            admit(&shared, &sender, Command::Speed(speed)).unwrap();
            state
                .command(receiver.try_recv().unwrap(), &shared)
                .unwrap();
            assert_eq!(state.speed, speed);
            assert!(!state.running);
            assert_eq!(state.solver.steps(), 0);
            assert_eq!(state.solver.time(), 0.0);
            assert_eq!(state.solver.grid().dt, grid.dt);
            assert_eq!(state.solver.grid().dx, grid.dx);
        }
        for invalid in [0, 129] {
            admit(&shared, &sender, Command::Speed(invalid)).unwrap();
            state
                .command(receiver.try_recv().unwrap(), &shared)
                .unwrap();
            assert_eq!(state.speed, 128);
        }
        admit(&shared, &sender, Command::Speed(7)).unwrap();
        admit(&shared, &sender, Command::Reset).unwrap();
        state
            .command(receiver.try_recv().unwrap(), &shared)
            .unwrap();
        state
            .command(receiver.try_recv().unwrap(), &shared)
            .unwrap();
        assert_eq!(state.speed, 7);
        assert!(!state.running);
        admit(&shared, &sender, Command::Step).unwrap();
        state
            .command(receiver.try_recv().unwrap(), &shared)
            .unwrap();
        assert_eq!(
            state.solver.steps(),
            1,
            "Step остаётся ровно одним физическим шагом"
        );
        assert_eq!(state.solver.time(), grid.dt);
        assert!(!state.running);
    }

    #[test]
    fn live_mailbox_coalesces_per_id_and_is_fair_and_bounded() {
        let (_, shared, _, _) = fixture();
        let mut gate = shared.gate.lock().unwrap();
        gate.enqueue_pose(request(1, 2, 0.011)).unwrap();
        gate.enqueue_pose(request(2, 3, 0.021)).unwrap();
        for i in 4..100 {
            gate.enqueue_pose(request(1, i, 0.012)).unwrap();
        }
        assert_eq!(gate.poses.len(), 2);
        assert_eq!(gate.order.len(), 2);
        let first = gate.take_pose().unwrap();
        assert_eq!((first.pose.id(), first.request_id), (1, 99));
        gate.enqueue_pose(request(1, 100, 0.013)).unwrap();
        assert_eq!(gate.take_pose().unwrap().pose.id(), 2);
        assert_eq!(gate.take_pose().unwrap().request_id, 100);
        for id in 3..1000 {
            assert!(gate.enqueue_pose(request(id, 101, 0.01)).is_err());
        }
        assert!(gate.poses.is_empty());
        assert!(gate.order.is_empty());
    }

    #[test]
    fn full_control_fifo_does_not_cancel_pose_or_advance_epoch() {
        let (_, shared, sender, receiver) = fixture();
        shared
            .gate
            .lock()
            .unwrap()
            .enqueue_pose(request(1, 2, 0.012))
            .unwrap();
        for _ in 0..32 {
            admit(&shared, &sender, Command::Play(false)).unwrap();
        }
        assert!(admit(&shared, &sender, Command::Reset).is_err());
        {
            let gate = shared.gate.lock().unwrap();
            assert_eq!((gate.epoch, gate.barriers, gate.poses.len()), (1, 0, 1));
        }
        receiver.try_recv().unwrap();
        admit(&shared, &sender, Command::Reset).unwrap();
        let mut gate = shared.gate.lock().unwrap();
        assert_eq!((gate.epoch, gate.barriers, gate.poses.len()), (2, 1, 0));
        assert!(gate.enqueue_pose(request(1, 3, 0.012)).is_err());
        let mut fresh = request(1, 4, 0.012);
        fresh.epoch = 2;
        assert!(
            gate.enqueue_pose(fresh).is_err(),
            "pending cold barrier blocks even fresh epoch"
        );
    }

    #[test]
    fn queued_cold_barriers_keep_live_blocked_until_last_rejection() {
        let (mut state, shared, sender, receiver) = fixture();
        shared
            .gate
            .lock()
            .unwrap()
            .enqueue_pose(request(1, 2, 0.012))
            .unwrap();
        let mut valid = (*state.scene).clone();
        valid.probes[0].id = 3;
        let mut invalid = valid.clone();
        invalid.probes[0].id = 4;
        invalid.domain.cell_m = f64::NAN;
        admit(
            &shared,
            &sender,
            Command::Apply {
                request_id: 3,
                scene: valid.clone(),
            },
        )
        .unwrap();
        admit(
            &shared,
            &sender,
            Command::Apply {
                request_id: 4,
                scene: invalid,
            },
        )
        .unwrap();
        {
            let mut gate = shared.gate.lock().unwrap();
            assert_eq!((gate.epoch, gate.barriers), (3, 2));
            assert!(gate.poses.is_empty());
            assert!(gate.order.is_empty());
            let mut newest = request(3, 5, 0.014);
            newest.epoch = 3;
            assert!(gate.enqueue_pose(newest).is_err());
        }
        state
            .command(receiver.try_recv().unwrap(), &shared)
            .unwrap();
        assert_eq!((state.epoch, state.revision, state.run), (2, 2, 2));
        assert_eq!(state.scene.as_ref(), &valid);
        {
            let mut gate = shared.gate.lock().unwrap();
            assert_eq!((gate.epoch, gate.barriers), (3, 1));
            assert_eq!(gate.known_ids, HashSet::from([2, 3]));
            let mut newest = request(3, 6, 0.014);
            newest.epoch = 3;
            assert!(
                gate.enqueue_pose(newest).is_err(),
                "второй barrier ещё ожидает обработки"
            );
        }
        state
            .command(receiver.try_recv().unwrap(), &shared)
            .unwrap();
        assert_eq!((state.epoch, state.revision, state.run), (3, 2, 2));
        assert_eq!(state.scene.as_ref(), &valid);
        let snapshot = shared.snapshot.lock().unwrap().as_ref().unwrap().clone();
        assert_eq!((snapshot.epoch, snapshot.revision, snapshot.run), (3, 2, 2));
        assert_eq!(snapshot.scene.as_ref(), &valid);
        {
            let mut gate = shared.gate.lock().unwrap();
            assert_eq!((gate.epoch, gate.barriers), (3, 0));
            assert_eq!(gate.known_ids, HashSet::from([2, 3]));
            // Ни старый gesture, ни ID отвергнутой сцены не становятся допустимыми.
            assert!(gate.enqueue_pose(request(3, 7, 0.014)).is_err());
            let mut rejected_id = request(4, 8, 0.014);
            rejected_id.epoch = 3;
            assert!(gate.enqueue_pose(rejected_id).is_err());
            let mut fresh = request(3, 9, 0.014);
            fresh.epoch = 3;
            gate.enqueue_pose(fresh).unwrap();
        }
        let fresh = shared.gate.lock().unwrap().take_pose().unwrap();
        let prepared = state.prepare_pose(&fresh.pose);
        state.commit_pose(fresh, prepared, &shared).unwrap();
        assert_eq!((state.epoch, state.revision, state.run), (3, 3, 2));
        assert_eq!(state.scene.probes[0].id, 3);
        assert_eq!(state.scene.probes[0].position.x, 0.014);
    }

    #[test]
    fn newer_live_request_during_prepare_does_not_cancel_intermediate_frame() {
        let (mut state, shared, _, _) = fixture();
        let first = request(1, 2, 0.012);
        let prepared = state.prepare_pose(&first.pose);
        shared
            .gate
            .lock()
            .unwrap()
            .enqueue_pose(request(1, 3, 0.014))
            .unwrap();
        state.commit_pose(first, prepared, &shared).unwrap();
        assert_eq!(state.revision, 2);
        assert_eq!(state.scene.probes[0].position.x, 0.012);
        assert_eq!(
            shared.snapshot.lock().unwrap().as_ref().unwrap().revision,
            2
        );
        // An intermediate physical step happens before taking the next request.
        assert!(state.advance(&shared));
        state.publish(&shared).unwrap();
        let next = shared.gate.lock().unwrap().take_pose().unwrap();
        let prepared = state.prepare_pose(&next.pose);
        state.commit_pose(next, prepared, &shared).unwrap();
        assert_eq!(state.revision, 3);
        assert_eq!(state.solver.steps(), 1);
        assert_eq!(state.scene.probes[0].position.x, 0.014);
    }

    #[test]
    fn cold_barrier_cancels_prepared_pose_and_rejected_cold_allows_fresh_pose() {
        let (mut state, shared, sender, receiver) = fixture();
        let first = request(1, 2, 0.012);
        let prepared = state.prepare_pose(&first.pose);
        let mut invalid = (*state.scene).clone();
        invalid.domain.cell_m = f64::NAN;
        admit(
            &shared,
            &sender,
            Command::Apply {
                request_id: 3,
                scene: invalid,
            },
        )
        .unwrap();
        state.commit_pose(first, prepared, &shared).unwrap();
        assert_eq!(state.revision, 1);
        assert_eq!(state.scene.probes[0].position.x, 0.01);
        state
            .command(receiver.try_recv().unwrap(), &shared)
            .unwrap();
        assert_eq!((state.epoch, state.revision, state.run), (2, 1, 1));
        assert_eq!(shared.snapshot.lock().unwrap().as_ref().unwrap().epoch, 2);
        let mut next = request(1, 4, 0.014);
        next.epoch = 2;
        shared.gate.lock().unwrap().enqueue_pose(next).unwrap();
        let next = shared.gate.lock().unwrap().take_pose().unwrap();
        let prepared = state.prepare_pose(&next.pose);
        state.commit_pose(next, prepared, &shared).unwrap();
        assert_eq!(state.revision, 2);
        assert_eq!(state.scene.probes[0].position.x, 0.014);
    }

    #[test]
    fn rejected_pose_keeps_running_and_is_not_reused_by_other_id() {
        let (mut state, shared, _, _) = fixture();
        state.running = true;
        let invalid = request(1, 2, f64::NAN);
        let prepared = state.prepare_pose(&invalid.pose);
        state.commit_pose(invalid, prepared, &shared).unwrap();
        assert!(state.running);
        assert_eq!(state.revision, 1);
        let valid = request(2, 3, 0.024);
        let prepared = state.prepare_pose(&valid.pose);
        state.commit_pose(valid, prepared, &shared).unwrap();
        assert!(state.running);
        assert_eq!(state.scene.probes[0].position.x, 0.01);
        assert_eq!(state.scene.probes[1].position.x, 0.024);
        assert!(state.advance(&shared));
    }

    #[test]
    fn pose_events_coalesce_without_losing_other_ids_and_files_stay_bounded() {
        let (_, shared, _, _) = fixture();
        for request_id in 2..100 {
            for object_id in [1, 2] {
                emit(
                    &shared,
                    Event::PoseResult {
                        epoch: 1,
                        request_id,
                        object_id,
                        result: Ok(2),
                    },
                );
            }
            emit(
                &shared,
                Event::FileSaved {
                    path: PathBuf::from(request_id.to_string()),
                },
            );
        }
        let events = shared.events.lock().unwrap();
        assert_eq!(events.len(), 18);
        for id in [1, 2] {
            assert!(events.iter().any(|e| matches!(e, Event::PoseResult { object_id, request_id: 99, .. } if *object_id == id)));
        }
        drop(events);
        emit(
            &shared,
            Event::Error {
                epoch: 2,
                request_id: 100,
                message: "invalid cold".into(),
            },
        );
        assert!(
            !shared
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, Event::PoseResult { .. }))
        );
    }

    #[test]
    fn display_decimation_preserves_peak_and_time_order() {
        let history = (0..10_000)
            .map(|i| (i as f64, vec![if i == 6543 { 100.0 } else { 0.0 }]))
            .collect();
        let out = plot_samples(&history, 0);
        assert!(out.len() <= 4096);
        assert!(out.contains(&[6543.0, 100.0]));
        assert!(out.windows(2).all(|p| p[0][0] <= p[1][0]));
    }
}
