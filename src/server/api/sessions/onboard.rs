//! External-conversation onboarding through the authenticated daemon.

use super::*;
use crate::session::onboarding::{self, Agent};
use axum::extract::Query;

fn default_agent() -> Agent {
    Agent::Codex
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalConversationQuery {
    #[serde(default = "default_agent")]
    agent: Agent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardConversationBody {
    #[serde(default = "default_agent")]
    agent: Agent,
    conversation_id: String,
    title: Option<String>,
    #[serde(default)]
    group: String,
}

fn blocked(state: &AppState) -> Option<axum::response::Response> {
    if state.cityhall_mode {
        Some(crate::server::api::cityhall_response())
    } else if state.read_only {
        Some(crate::server::api::read_only_response())
    } else {
        None
    }
}

pub async fn list_external_conversations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ExternalConversationQuery>,
) -> axum::response::Response {
    if let Some(response) = blocked(&state) {
        return response;
    }
    let profile = state.profile.clone();
    match tokio::task::spawn_blocking(move || onboarding::list_available(&profile, query.agent)).await {
        Ok(Ok(conversations)) => Json(serde_json::json!({"conversations": conversations.into_iter().map(|c| serde_json::json!({
            "conversation_id": c.session_id, "agent": c.agent, "path": c.cwd,
            "title": c.title, "last_modified_ms": c.last_modified_ms, "cwd_exists": c.cwd_exists
        })).collect::<Vec<_>>()})).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":"conversation_discovery_failed"}))).into_response(),
    }
}

pub async fn onboard_conversation(
    State(state): State<Arc<AppState>>,
    body: Result<Json<OnboardConversationBody>, axum::extract::rejection::JsonRejection>,
) -> axum::response::Response {
    if let Some(response) = blocked(&state) {
        return response;
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    if !crate::session::is_valid_session_id(&body.conversation_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"invalid_conversation_id"})),
        )
            .into_response();
    }
    for (value, label) in [
        (body.title.as_deref().unwrap_or(""), "title"),
        (body.group.as_str(), "group"),
    ] {
        if let Err(message) = validate_display_label(value, label) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":"invalid_label", "message":message})),
            )
                .into_response();
        }
    }
    let profile = state.profile.clone();
    let conversation_id = body.conversation_id.clone();
    let discovered =
        tokio::task::spawn_blocking(move || onboarding::discover(&profile, body.agent)).await;
    let conversation = match discovered {
        Ok(Ok(conversations)) => match conversations
            .into_iter()
            .find(|c| c.session_id == conversation_id)
        {
            Some(conversation) => conversation,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error":"conversation_not_found"})),
                )
                    .into_response()
            }
        },
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":"conversation_discovery_failed"})),
            )
                .into_response()
        }
    };
    let mut instance = match onboarding::build_instance(&conversation, body.title.as_deref(), &body.group) {
        Ok(instance) => instance,
        Err(error) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"conversation_not_resumable", "message":error.to_string()}))).into_response(),
    };
    instance.source_profile = state.profile.clone();
    let profile = state.profile.clone();
    // Hold the cache write lock through persistence and publication, as on create.
    let mut instances = state.instances.write().await;
    let registered = tokio::task::spawn_blocking(move || {
        onboarding::register(&profile, &conversation, &instance)
    })
    .await;
    match registered {
        Ok(Ok((mut instance, created))) => {
            instance.source_profile = state.profile.clone();
            if !instances.iter().any(|i| i.id == instance.id) {
                instances.push(instance.clone());
                state
                    .mutation_epoch
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (
                status,
                Json(serde_json::json!({
                    "session_id": instance.id, "conversation_id": conversation_id,
                    "agent": instance.tool, "title": instance.title, "path": instance.project_path,
                    "created": created, "view": instance.view
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"conversation_onboarding_failed"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        routing::{get, post},
        Router,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    fn router(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/external-conversations",
                get(list_external_conversations),
            )
            .route("/api/sessions/onboard", post(onboard_conversation))
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

    #[tokio::test]
    #[serial_test::serial]
    async fn onboard_routes_preserve_history_and_publish_one_session() {
        let temp = tempfile::tempdir().unwrap();
        let _home = crate::session::test_support::isolate_home(temp.path());
        crate::session::create_profile("default").unwrap();
        let codex = temp.path().join("codex");
        let _env = crate::session::test_support::EnvGuard::set(&[("CODEX_HOME", &codex)]);
        let sessions = codex.join("sessions/2026/09/06");
        std::fs::create_dir_all(&sessions).unwrap();
        let native = "11111111-2222-4333-8444-555555555555";
        std::fs::write(
            sessions.join(format!("rollout-date-{native}.jsonl")),
            format!(
                "{}\n",
                json!({
                    "type":"session_meta", "payload":{"id":native,"cwd":temp.path(),"source":"cli"}
                })
            ),
        )
        .unwrap();
        let mut state = crate::server::test_support::build_test_app_state(vec![]);
        Arc::get_mut(&mut state).unwrap().profile = "default".to_string();
        let app = router(state.clone());
        let (status, listed) = request(
            &app,
            "GET",
            "/api/external-conversations?agent=codex",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["conversations"][0]["conversation_id"], native);
        let body = json!({"conversation_id":native,"title":"Existing work","group":"outside"});
        let (status, created) = request(&app, "POST", "/api/sessions/onboard", body.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["conversation_id"], native);
        assert_eq!(created["created"], true);
        let (status, repeated) = request(&app, "POST", "/api/sessions/onboard", body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(repeated["session_id"], created["session_id"]);
        assert_eq!(repeated["created"], false);
        let rows = Storage::open_unwatched("default").unwrap().load().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].resume_intent,
            crate::session::ResumeIntent::Use(native.to_string())
        );
        assert_eq!(rows[0].project_path, temp.path().to_str().unwrap());
        assert!(!rows[0].tmux_session().unwrap().exists());
        let cached = state.instances.read().await;
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].id, rows[0].id);
        assert_eq!(cached[0].source_profile, "default");
        drop(cached);
        assert_eq!(
            state
                .mutation_epoch
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let (_, listed) = request(&app, "GET", "/api/external-conversations", Value::Null).await;
        assert_eq!(listed["conversations"], json!([]));
        let (status, _) = request(
            &app,
            "POST",
            "/api/sessions/onboard",
            json!({"conversation_id":native,"path":"/override"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = request(
            &app,
            "POST",
            "/api/sessions/onboard",
            json!({"conversation_id":"missing"}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            Storage::open_unwatched("default")
                .unwrap()
                .load()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn onboard_routes_reject_restricted_daemons() {
        let _home = crate::session::test_support::isolate_app_dir();
        for cityhall in [false, true] {
            let mut state = crate::server::test_support::build_test_app_state(vec![]);
            let state_mut = Arc::get_mut(&mut state).unwrap();
            state_mut.cityhall_mode = cityhall;
            state_mut.read_only = !cityhall;
            let app = router(state);
            for (method, path) in [
                ("GET", "/api/external-conversations"),
                ("POST", "/api/sessions/onboard"),
            ] {
                let (status, _) =
                    request(&app, method, path, json!({"conversation_id":"native-id"})).await;
                assert_eq!(status, StatusCode::FORBIDDEN);
            }
        }
    }
}
