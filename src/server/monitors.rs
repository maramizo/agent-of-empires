//! Persistent Python monitors owned by the daemon's profile.
use super::AppState;
use crate::session::{acquire_storage_flock, atomic_write, try_acquire_storage_flock};
use anyhow::{bail, Context, Result};
use axum::{
    extract::{Path as ApiPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, Semaphore},
};
use tokio_util::sync::CancellationToken;

const OUTPUT_CAP: usize = 32 * 1024;
const SDK: &str = include_str!("../../monitor-sdk/aoe_monitor.py");
fn now() -> u64 {
    crate::util::system_time_to_ms(std::time::SystemTime::now())
}
fn yes() -> bool {
    true
}
fn python() -> String {
    "python3".into()
}
fn timeout() -> u64 {
    300
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    pub name: String,
    pub script_path: String,
    pub cadence: String,
    #[serde(default = "python")]
    pub python: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "yes")]
    pub dry_mode: bool,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Monitor {
    pub id: String,
    #[serde(flatten)]
    pub config: MonitorConfig,
    pub created_at_ms: u64,
    pub next_run_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Run {
    pub id: String,
    pub monitor_id: String,
    pub dry_mode: bool,
    pub status: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
}

#[derive(Deserialize, Serialize)]
struct Store {
    schema_version: u32,
    monitors: Vec<Monitor>,
    runs: Vec<Run>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            schema_version: 1,
            monitors: vec![],
            runs: vec![],
        }
    }
}

pub struct Runtime {
    pub url: String,
    active: Mutex<HashMap<String, (String, CancellationToken)>>,
    slots: Arc<Semaphore>,
}
impl Runtime {
    pub fn new(url: String) -> Self {
        Self {
            url,
            active: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(4)),
        }
    }
}

fn cadence_ms(value: &str) -> Result<u64> {
    let (number, unit) = value.trim().split_at(
        value
            .trim()
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(value.trim().len()),
    );
    let multiplier = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => bail!("cadence must be an integer followed by s, m, h, or d"),
    };
    let millis = number
        .parse::<u64>()?
        .checked_mul(multiplier)
        .context("cadence overflow")?;
    if !(1_000..=31 * 86_400_000).contains(&millis) {
        bail!("cadence must be between 1s and 31d");
    }
    Ok(millis)
}
impl MonitorConfig {
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.len() > 200
            || self.name.chars().any(char::is_control)
        {
            bail!("invalid monitor name");
        }
        cadence_ms(&self.cadence)?;
        let path = Path::new(&self.script_path);
        if !path.is_absolute() || !path.is_file() {
            bail!("script_path must be an existing absolute file path on the daemon host");
        }
        if let Some(cwd) = &self.working_directory {
            if !Path::new(cwd).is_absolute() || !Path::new(cwd).is_dir() {
                bail!("working_directory must be an existing absolute directory");
            }
        }
        if self.python.is_empty()
            || self.python.contains('\0')
            || self
                .args
                .iter()
                .any(|a| a.contains('\0') || a == "--dry-mode")
        {
            bail!("invalid Python executable or args; dry_mode is managed by AoE");
        }
        if !(1..=86400).contains(&self.timeout_seconds) {
            bail!("timeout_seconds must be between 1 and 86400");
        }
        Ok(())
    }
}
fn directory(profile: &str) -> Result<PathBuf> {
    Ok(crate::session::get_profile_dir_path(profile)?.join("monitors"))
}
fn transact<T>(profile: &str, write: bool, f: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
    let dir = directory(profile)?;
    std::fs::create_dir_all(&dir)?;
    let _lock = acquire_storage_flock(&dir, ".store.lock")?;
    let path = dir.join("monitors.json");
    let mut store = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Store>(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::default(),
        Err(e) => return Err(e.into()),
    };
    if store.schema_version != 1 {
        bail!("unsupported monitor schema version");
    }
    let result = f(&mut store)?;
    if write {
        atomic_write(&path, &serde_json::to_vec_pretty(&store)?)?;
    }
    Ok(result)
}
pub(crate) fn initialize_profile(profile: &str) -> Result<()> {
    transact(profile, true, |_| Ok(()))
}

fn blocked(state: &AppState) -> Option<Response> {
    if state.cityhall_mode {
        Some(super::api::cityhall_response())
    } else if state.read_only {
        Some(super::api::read_only_response())
    } else {
        None
    }
}
fn failure(status: StatusCode, error: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error":error.to_string()}))).into_response()
}
async fn db<T: Send + 'static>(
    profile: String,
    write: bool,
    f: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(move || transact(&profile, write, f)).await?
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    if let Some(r) = blocked(&state) {
        return r;
    }
    match db(state.profile.clone(), false, |s| Ok(s.monitors.clone())).await {
        Ok(monitors) => (Json(json!({"monitors":monitors}))).into_response(),
        Err(e) => failure(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(config): Json<MonitorConfig>,
) -> Response {
    if let Some(r) = blocked(&state) {
        return r;
    }
    if let Err(e) = config.validate() {
        return failure(StatusCode::BAD_REQUEST, e);
    }
    match db(state.profile.clone(), true, move |s| {
        if let Some(m) = s.monitors.iter().find(|m| m.config.name == config.name) {
            if m.config == config {
                return Ok((m.clone(), false));
            }
            bail!("monitor name already exists; update the existing monitor");
        }
        if s.monitors.len() >= 100 {
            bail!("profile monitor limit reached (100)");
        }
        let timestamp = now();
        let next = timestamp + cadence_ms(&config.cadence)?;
        let m = Monitor {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            created_at_ms: timestamp,
            next_run_at_ms: next,
        };
        s.monitors.push(m.clone());
        Ok((m, true))
    })
    .await
    {
        Ok((m, created)) => (
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            Json(json!({"monitor":m,"created":created})),
        )
            .into_response(),
        Err(e) => failure(StatusCode::CONFLICT, e),
    }
}
pub async fn update(
    State(state): State<Arc<AppState>>,
    ApiPath(id): ApiPath<String>,
    Json(patch): Json<Value>,
) -> Response {
    if let Some(r) = blocked(&state) {
        return r;
    }
    match db(state.profile.clone(), true, move |s| {
        let index = s
            .monitors
            .iter()
            .position(|m| m.id == id)
            .context("monitor not found")?;
        let mut config = serde_json::to_value(&s.monitors[index].config)?;
        for (key, value) in patch.as_object().context("patch must be an object")? {
            config[key] = value.clone();
        }
        let config: MonitorConfig = serde_json::from_value(config)?;
        config.validate()?;
        if s.monitors
            .iter()
            .any(|m| m.id != id && m.config.name == config.name)
        {
            bail!("monitor name already exists");
        }
        let m = &mut s.monitors[index];
        if m.config.cadence != config.cadence || (!m.config.enabled && config.enabled) {
            m.next_run_at_ms = now() + cadence_ms(&config.cadence)?;
        }
        m.config = config;
        Ok(m.clone())
    })
    .await
    {
        Ok(m) => Json(json!({"monitor":m})).into_response(),
        Err(e) => failure(StatusCode::BAD_REQUEST, e),
    }
}
pub async fn delete(State(state): State<Arc<AppState>>, ApiPath(id): ApiPath<String>) -> Response {
    if let Some(r) = blocked(&state) {
        return r;
    }
    let profile = state.profile.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::session::validate_instance_id(&id)?;
        let dir = directory(&profile)?;
        let _run = try_acquire_storage_flock(&dir, &format!("{id}.run.lock"))?
            .context("monitor is running; cancel it first")?;
        transact(&profile, true, |s| {
            let index = s
                .monitors
                .iter()
                .position(|m| m.id == id)
                .context("monitor not found")?;
            s.monitors.remove(index);
            s.runs.retain(|r| r.monitor_id != id);
            Ok(())
        })
    })
    .await;
    match result {
        Ok(Ok(())) => Json(json!({"deleted":true})).into_response(),
        other => failure(StatusCode::CONFLICT, format!("{other:?}")),
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOptions {
    dry_mode: Option<bool>,
}
pub async fn run_now(
    State(state): State<Arc<AppState>>,
    ApiPath(id): ApiPath<String>,
    Json(options): Json<RunOptions>,
) -> Response {
    if let Some(r) = blocked(&state) {
        return r;
    }
    match launch(state, id, options.dry_mode, false).await {
        Ok(run) => (StatusCode::ACCEPTED, Json(json!({"run":run}))).into_response(),
        Err(e) => failure(StatusCode::CONFLICT, e),
    }
}
pub async fn runs(State(state): State<Arc<AppState>>, ApiPath(id): ApiPath<String>) -> Response {
    if let Some(r) = blocked(&state) {
        return r;
    }
    match db(state.profile.clone(), false, move |s| {
        Ok(s.runs
            .iter()
            .filter(|r| r.monitor_id == id)
            .cloned()
            .collect::<Vec<_>>())
    })
    .await
    {
        Ok(runs) => Json(json!({"runs":runs})).into_response(),
        Err(e) => failure(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
pub async fn cancel(State(state): State<Arc<AppState>>, ApiPath(id): ApiPath<String>) -> Response {
    if let Some(r) = blocked(&state) {
        return r;
    }
    if let Some((_, token)) = state.monitors.active.lock().await.get(&id) {
        token.cancel();
        Json(json!({"cancellation_requested":true})).into_response()
    } else {
        failure(StatusCode::NOT_FOUND, "no active run on this daemon")
    }
}

async fn launch(
    state: Arc<AppState>,
    id: String,
    mode: Option<bool>,
    scheduled: bool,
) -> Result<Run> {
    if state.shutdown.is_cancelled() {
        bail!("daemon is shutting down");
    }
    let slot = state
        .monitors
        .slots
        .clone()
        .try_acquire_owned()
        .context("all four monitor slots are busy")?;
    let profile = state.profile.clone();
    let claim_id = id.clone();
    let (monitor, run, lease) = tokio::task::spawn_blocking(move || {
        crate::session::validate_instance_id(&claim_id)?;
        let dir = directory(&profile)?;
        std::fs::create_dir_all(&dir)?;
        let lease = try_acquire_storage_flock(&dir, &format!("{claim_id}.run.lock"))?
            .context("monitor already running")?;
        let (monitor, run) = transact(&profile, true, |s| {
            let m = s
                .monitors
                .iter_mut()
                .find(|m| m.id == claim_id)
                .context("monitor not found")?;
            if scheduled && (!m.config.enabled || m.next_run_at_ms > now()) {
                bail!("monitor is not due");
            }
            let run = Run {
                id: uuid::Uuid::new_v4().to_string(),
                monitor_id: m.id.clone(),
                dry_mode: mode.unwrap_or(m.config.dry_mode),
                status: "running".into(),
                started_at_ms: now(),
                finished_at_ms: None,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                output_truncated: false,
            };
            m.next_run_at_ms = run.started_at_ms + cadence_ms(&m.config.cadence)?;
            // A previous daemon may have died without recording completion.
            for old in s
                .runs
                .iter_mut()
                .filter(|r| r.monitor_id == claim_id && r.status == "running")
            {
                old.status = "interrupted".into();
                old.finished_at_ms = Some(now());
            }
            s.runs.push(run.clone());
            while s.runs.iter().filter(|r| r.monitor_id == claim_id).count() > 10 {
                let index = s
                    .runs
                    .iter()
                    .position(|r| r.monitor_id == claim_id)
                    .unwrap();
                s.runs.remove(index);
            }
            Ok((m.clone(), run))
        })?;
        Ok::<_, anyhow::Error>((monitor, run, lease))
    })
    .await??;
    let cancel = state.shutdown.child_token();
    state
        .monitors
        .active
        .lock()
        .await
        .insert(id.clone(), (run.id.clone(), cancel.clone()));
    let accepted = run.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let _lease = lease;
        let mut run = run;
        if let Err(error) = execute(&state, &monitor, &mut run, cancel).await {
            run.status = "failed".into();
            run.stderr = format!("{error:#}");
        }
        run.finished_at_ms = Some(now());
        let finished_id = run.id.clone();
        if let Err(error) = db(state.profile.clone(), true, move |s| {
            if let Some(saved) = s.runs.iter_mut().find(|r| r.id == run.id) {
                *saved = run;
            }
            drop(_lease);
            drop(_slot);
            Ok(())
        })
        .await
        {
            tracing::error!("Could not persist monitor completion: {error:#}");
        }
        let mut active = state.monitors.active.lock().await;
        if active
            .get(&id)
            .is_some_and(|(run_id, _)| run_id == &finished_id)
        {
            active.remove(&id);
        }
    });
    Ok(accepted)
}
async fn read_output(mut reader: impl tokio::io::AsyncRead + Unpin) -> (String, bool) {
    let mut tail = Vec::new();
    let mut buffer = [0; 4096];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.extend_from_slice(&buffer[..n]);
                if tail.len() > OUTPUT_CAP {
                    truncated = true;
                    tail.drain(..tail.len() - OUTPUT_CAP);
                }
            }
        }
    }
    (String::from_utf8_lossy(&tail).into_owned(), truncated)
}
async fn execute(
    state: &AppState,
    monitor: &Monitor,
    run: &mut Run,
    cancel: CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        run.status = "cancelled".into();
        return Ok(());
    }
    monitor.config.validate()?;
    let sdk = directory(&state.profile)?.join("sdk");
    std::fs::create_dir_all(&sdk)?;
    atomic_write(&sdk.join("aoe_monitor.py"), SDK.as_bytes())?;
    let binary = crate::process::current_exe_for_spawn()?;
    let token = if run.dry_mode {
        String::new()
    } else {
        state
            .token_manager
            .current_token()
            .await
            .unwrap_or_default()
    };
    let mut command = std::process::Command::new(&monitor.config.python);
    command
        .arg(&monitor.config.script_path)
        .args(&monitor.config.args);
    if run.dry_mode {
        command.arg("--dry-mode");
    }
    command.current_dir(
        monitor
            .config
            .working_directory
            .as_deref()
            .map(Path::new)
            .unwrap_or_else(|| Path::new(&monitor.config.script_path).parent().unwrap()),
    );
    for entry in
        crate::session::config::profile_config::resolve_config_or_warn(&state.profile).environment
    {
        if let Some((key, value)) = entry.split_once('=') {
            command.env(key, value);
        }
    }
    let mut python_paths = vec![sdk];
    if let Some(existing) = std::env::var_os("PYTHONPATH") {
        python_paths.extend(std::env::split_paths(&existing));
    }
    command
        .env("PYTHONPATH", std::env::join_paths(python_paths)?)
        .env("PYTHONUNBUFFERED", "1")
        .env("AOE_MONITOR_BIN", binary)
        .env("AOE_PROFILE", &state.profile)
        .env("AGENT_OF_EMPIRES_PROFILE", &state.profile)
        .env("AOE_MONITOR_ID", &monitor.id)
        .env("AOE_MONITOR_RUN_ID", &run.id)
        .env("AOE_MONITOR_DRY_MODE", if run.dry_mode { "1" } else { "0" })
        .env("AOE_DAEMON_URL", &state.monitors.url)
        .env("AOE_DAEMON_TOKEN", &token)
        .env_remove("AOE_INSTANCE_ID")
        .env_remove("AOE_HOOK_BIN")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::process::configure_process_group(&mut command);
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .context("starting Python monitor")?;
    let pid = child.id().context("Python monitor PID unavailable")?;
    let mut out = tokio::spawn(read_output(child.stdout.take().unwrap()));
    let mut err = tokio::spawn(read_output(child.stderr.take().unwrap()));
    let status = tokio::select! {
        status=child.wait()=> {let status=status?;run.exit_code=status.code();if status.success(){"succeeded"}else{"failed"}},
        _=cancel.cancelled()=>"cancelled",
        _=tokio::time::sleep(Duration::from_secs(monitor.config.timeout_seconds))=>"timed_out",
    };
    // Always reap the process group, including children left behind by an exited script.
    crate::process::kill_monitor_process_group(pid);
    let _ = child.kill().await;
    let _ = child.wait().await;
    let (stdout, ot) = match tokio::time::timeout(Duration::from_secs(1), &mut out).await {
        Ok(result) => result?,
        Err(_) => {
            out.abort();
            ("Output pipe remained open after process exit".into(), true)
        }
    };
    let (stderr, et) = match tokio::time::timeout(Duration::from_secs(1), &mut err).await {
        Ok(result) => result?,
        Err(_) => {
            err.abort();
            ("Output pipe remained open after process exit".into(), true)
        }
    };
    run.stdout = if token.is_empty() {
        stdout
    } else {
        stdout.replace(&token, "[redacted]")
    };
    run.stderr = if token.is_empty() {
        stderr
    } else {
        stderr.replace(&token, "[redacted]")
    };
    run.output_truncated = ot || et;
    run.status = status.into();
    Ok(())
}
pub fn start(state: Arc<AppState>) {
    if state.read_only || state.cityhall_mode {
        return;
    }
    tokio::spawn(async move {
        let profile = state.profile.clone();
        let _ = tokio::task::spawn_blocking(move || recover(&profile)).await;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {_=state.shutdown.cancelled()=>break,_=tick.tick()=>{}}
            match db(state.profile.clone(), false, |s| {
                let mut due = s
                    .monitors
                    .iter()
                    .filter(|m| m.config.enabled && m.next_run_at_ms <= now())
                    .collect::<Vec<_>>();
                due.sort_by_key(|m| m.next_run_at_ms);
                Ok(due.into_iter().map(|m| m.id.clone()).collect::<Vec<_>>())
            })
            .await
            {
                Ok(ids) => {
                    for id in ids {
                        let _ = launch(state.clone(), id, None, true).await;
                    }
                }
                Err(e) => tracing::warn!("Monitor scheduler could not load its store: {e:#}"),
            }
        }
    });
}

fn recover(profile: &str) -> Result<()> {
    let ids = transact(profile, false, |s| {
        Ok(s.runs
            .iter()
            .filter(|r| r.status == "running")
            .map(|r| r.monitor_id.clone())
            .collect::<Vec<_>>())
    })?;
    for id in ids {
        if let Some(_lease) =
            try_acquire_storage_flock(&directory(profile)?, &format!("{id}.run.lock"))?
        {
            transact(profile, true, |s| {
                for run in s
                    .runs
                    .iter_mut()
                    .filter(|r| r.monitor_id == id && r.status == "running")
                {
                    run.status = "interrupted".into();
                    run.finished_at_ms = Some(now());
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}

pub async fn wait_for_shutdown(state: &AppState) {
    while !state.monitors.active.lock().await.is_empty() {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        routing::{get, patch, post},
        Router,
    };
    use tower::ServiceExt;

    fn app(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/monitors", get(list).post(create))
            .route("/monitors/{id}", patch(update).delete(delete))
            .route("/monitors/{id}/run", post(run_now))
            .route("/monitors/{id}/runs", get(runs))
            .route("/monitors/{id}/cancel", post(cancel))
            .with_state(state)
    }
    async fn request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    async fn finished(profile: &str, id: &str) -> Run {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            let run = transact(profile, false, |s| {
                Ok(s.runs.iter().find(|r| r.id == id).cloned())
            })
            .unwrap()
            .unwrap();
            if run.status != "running" {
                return run;
            }
            assert!(tokio::time::Instant::now() < deadline, "run did not finish");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    #[tokio::test]
    #[serial_test::serial]
    async fn monitor_lifecycle_runs_python_and_handles_dry_live_failure_timeout_and_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let _home = crate::session::test_support::isolate_home(temp.path());
        crate::session::create_profile("test").unwrap();
        let state = crate::server::test_support::build_test_app_state(vec![]);
        let app = app(state.clone());
        let script = temp.path().join("monitor.py");
        std::fs::write(&script,"import sys, os\nimport aoe_monitor\nprint('--dry-mode' in sys.argv, os.environ['AOE_MONITOR_DRY_MODE'])\n").unwrap();
        let config = json!({"name":"test","script_path":script,"cadence":"1s","enabled":false});
        let (status, created) = request(&app, "POST", "/monitors", config.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["monitor"]["id"].as_str().unwrap();
        let (status, repeated) = request(&app, "POST", "/monitors", config).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(repeated["monitor"]["id"], id);
        let path = format!("/monitors/{id}/run");
        for dry in [true, false] {
            let (status, result) = request(&app, "POST", &path, json!({"dry_mode":dry})).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{result}");
            let run = finished("test", result["run"]["id"].as_str().unwrap()).await;
            assert_eq!(run.status, "succeeded", "{}", run.stderr);
            assert_eq!(run.dry_mode, dry);
            assert_eq!(run.stdout.trim(), if dry { "True 1" } else { "False 0" });
        }
        std::fs::write(
            &script,
            "import sys\nprint('x'*100000)\nprint('failure',file=sys.stderr)\nsys.exit(7)\n",
        )
        .unwrap();
        let run = launch(state.clone(), id.into(), None, false).await.unwrap();
        let run = finished("test", &run.id).await;
        assert_eq!(run.status, "failed");
        assert_eq!(run.exit_code, Some(7));
        assert!(run.output_truncated);
        assert_eq!(run.stdout.len(), OUTPUT_CAP);
        assert!(run.stderr.contains("failure"));
        std::fs::write(&script, "import time\ntime.sleep(60)\n").unwrap();
        let (status, _) = request(
            &app,
            "PATCH",
            &format!("/monitors/{id}"),
            json!({"timeout_seconds":1}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let run = launch(state.clone(), id.into(), None, false).await.unwrap();
        assert!(launch(state.clone(), id.into(), None, false).await.is_err());
        assert_eq!(
            request(&app, "DELETE", &format!("/monitors/{id}"), json!({}))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(finished("test", &run.id).await.status, "timed_out");
        let run = launch(state.clone(), id.into(), None, false).await.unwrap();
        assert_eq!(
            request(&app, "POST", &format!("/monitors/{id}/cancel"), json!({}))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(finished("test", &run.id).await.status, "cancelled");
        let (_, history) = request(&app, "GET", &format!("/monitors/{id}/runs"), json!({})).await;
        assert_eq!(history["runs"].as_array().unwrap().len(), 5);
        assert_eq!(
            request(&app, "DELETE", &format!("/monitors/{id}"), json!({}))
                .await
                .0,
            StatusCode::OK
        );
        assert!(script.exists());
        state.shutdown.cancel();
        wait_for_shutdown(&state).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn monitor_scheduler_respects_pause_and_recovers_interrupted_runs() {
        let temp = tempfile::tempdir().unwrap();
        let _home = crate::session::test_support::isolate_home(temp.path());
        crate::session::create_profile("test").unwrap();
        let state = crate::server::test_support::build_test_app_state(vec![]);
        let app = app(state.clone());
        let script = temp.path().join("monitor.py");
        std::fs::write(&script, "print('scheduled')\n").unwrap();
        let (_, created) = request(
            &app,
            "POST",
            "/monitors",
            json!({"name":"scheduled","script_path":script,"cadence":"1h","enabled":false}),
        )
        .await;
        let id = created["monitor"]["id"].as_str().unwrap().to_string();
        transact("test", true, |s| {
            s.monitors[0].next_run_at_ms = 0;
            Ok(())
        })
        .unwrap();
        assert!(launch(state.clone(), id.clone(), None, true).await.is_err());
        transact("test", true, |s| {
            s.monitors[0].config.enabled = true;
            Ok(())
        })
        .unwrap();
        start(state.clone());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let run = loop {
            if let Some(run) = transact("test", false, |s| Ok(s.runs.first().cloned())).unwrap() {
                break run;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert_eq!(finished("test", &run.id).await.status, "succeeded");
        state.shutdown.cancel();
        wait_for_shutdown(&state).await;
        transact("test", true, |s| {
            s.runs[0].status = "running".into();
            Ok(())
        })
        .unwrap();
        recover("test").unwrap();
        assert_eq!(
            transact("test", false, |s| Ok(s.runs[0].status.clone())).unwrap(),
            "interrupted"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn monitor_routes_reject_restricted_daemons_and_invalid_cadences() {
        for (value, valid) in [
            ("1s", true),
            ("5m", true),
            ("24h", true),
            ("31d", true),
            ("0s", false),
            ("32d", false),
            ("every minute", false),
            ("-1s", false),
            ("18446744073709551615d", false),
        ] {
            assert_eq!(cadence_ms(value).is_ok(), valid, "{value}");
        }
        let _home = crate::session::test_support::isolate_app_dir();
        for cityhall in [false, true] {
            let mut state = crate::server::test_support::build_test_app_state(vec![]);
            let s = Arc::get_mut(&mut state).unwrap();
            s.cityhall_mode = cityhall;
            s.read_only = !cityhall;
            let app = app(state);
            for (method, path, body) in [
                ("GET", "/monitors", json!({})),
                (
                    "POST",
                    "/monitors",
                    json!({"name":"x","script_path":"/x","cadence":"1m"}),
                ),
                ("POST", "/monitors/id/run", json!({})),
                ("PATCH", "/monitors/id", json!({"enabled":true})),
                ("DELETE", "/monitors/id", json!({})),
                ("GET", "/monitors/id/runs", json!({})),
                ("POST", "/monitors/id/cancel", json!({})),
            ] {
                assert_eq!(
                    request(&app, method, path, body).await.0,
                    StatusCode::FORBIDDEN
                );
            }
        }
    }
}
