mod admin;
mod auth;
mod hosts;
mod middleware;
mod portal;
mod proxy;
mod registry;
mod session;

use anyhow::Result;
use axum::{
    routing::{delete, get, post},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use std::{sync::Arc, time::Duration};
use tower_http::trace::TraceLayer;
use tracing::info;

/// Shared application state threaded through all handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::MultiplexedConnection,
    pub session: Arc<session::SessionManager>,
    pub oidc: Arc<auth::OidcClient>,
    pub registry: Arc<registry::BackendRegistry>,
    pub config: Arc<Config>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub gateway_port: u16,
    pub gateway_origin: String,
    pub jwt_secret: String,
    pub admin_subs: Vec<String>,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Config {
            gateway_port: std::env::var("GATEWAY_PORT")
                .unwrap_or_else(|_| "8080".into())
                .parse()?,
            gateway_origin: std::env::var("GATEWAY_ORIGIN")
                .unwrap_or_else(|_| "http://localhost:8080".into()),
            jwt_secret: std::env::var("JWT_SECRET")
                .expect("JWT_SECRET env var required"),
            admin_subs: std::env::var("ADMIN_SUBS")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "streamio_gateway=info,tower_http=info".parse().unwrap()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);

    // PostgreSQL connection pool
    let db = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL required"))
        .await?;

    // Run migrations
    // Run migrations — split multi-statement scripts and strip comments
    for migration in [
        include_str!("../migrations/001_init.sql"),
        include_str!("../migrations/002_vm_columns.sql"),
        include_str!("../migrations/003_hosts.sql"),
        include_str!("../migrations/004_known_users.sql"),
    ] {
        // Strip SQL comments (-- to end of line) then split on ;
        let cleaned: String = migration
            .lines()
            .map(|line| {
                if let Some(pos) = line.find("--") {
                    &line[..pos]
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        for stmt in cleaned.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            sqlx::query(stmt).execute(&db).await?;
        }
    }

    // Redis connection
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".into());
    let redis_client = redis::Client::open(redis_url)?;
    let redis = redis_client.get_multiplexed_async_connection().await?;

    // OIDC client
    let oidc = Arc::new(
        auth::OidcClient::discover(
            std::env::var("OIDC_ISSUER_URL").expect("OIDC_ISSUER_URL required"),
            std::env::var("OIDC_CLIENT_ID").expect("OIDC_CLIENT_ID required"),
            std::env::var("OIDC_CLIENT_SECRET").expect("OIDC_CLIENT_SECRET required"),
            std::env::var("OIDC_REDIRECT_URI").expect("OIDC_REDIRECT_URI required"),
        )
        .await?,
    );

    // Session manager (JWT)
    let session = Arc::new(session::SessionManager::new(config.jwt_secret.clone()));

    // Backend registry (legacy direct-backend pool)
    let registry = Arc::new(registry::BackendRegistry::new(db.clone()));

    let state = AppState {
        db: db.clone(),
        redis,
        session,
        oidc,
        registry: registry.clone(),
        config: config.clone(),
    };

    // Background tasks
    tokio::spawn(registry::health_poll_task(registry, db.clone()));
    tokio::spawn(hosts::host_health_poll_task(db));

    let app = Router::new()
        // ── Public: auth ──
        .route("/auth/login", get(auth::login_handler))
        .route("/auth/callback", get(auth::callback_handler))
        .route("/auth/logout", get(auth::logout_handler))
        .route("/healthz", get(|| async { "ok" }))

        // ── User portal ──
        .route("/portal", get(portal::portal_ui))
        .route("/api/me", get(portal::me))
        .route("/api/me/vdis", get(portal::my_vdis))
        .route("/api/me/connect", post(portal::connect))
        .route("/api/me/disconnect", post(portal::disconnect))

        // ── VDI session stream proxy ──
        .route("/vdi/:session_id", get(portal::vdi_stream_ui))
        .route("/vdi/:session_id/ws", get(portal::vdi_ws_proxy))

        // ── Main entry + legacy stream ──
        .route("/", get(proxy::index_handler))
        .route("/stream", get(proxy::stream_handler))
        .route("/ws", get(proxy::ws_handler))

        // ── Admin UI ──
        .route("/admin", get(admin::admin_ui_handler))

        // ── Admin API: hosts (session manager agents) ──
        .route("/admin/api/hosts", get(hosts::list_hosts))
        .route("/admin/api/hosts", post(hosts::create_host))
        .route("/admin/api/hosts/:id", delete(hosts::delete_host))

        // ── Admin API: known users + user-host assignments ──
        .route("/admin/api/known-users", get(hosts::list_known_users))
        .route("/admin/api/host-assignments", get(hosts::list_host_assignments))
        .route("/admin/api/host-assignments", post(hosts::create_host_assignment))
        .route("/admin/api/host-assignments/:sub/:host_id", delete(hosts::delete_host_assignment))

        // ── Admin API: VDI sessions ──
        .route("/admin/api/vdi-sessions", get(hosts::list_vdi_sessions))
        .route("/admin/api/vdi-sessions/:id", delete(hosts::kill_vdi_session))

        // ── Admin API: legacy backends ──
        .route("/admin/api/backends", get(admin::list_backends))
        .route("/admin/api/users", get(admin::list_users))
        .route("/admin/api/assignments", post(admin::create_assignment))
        .route("/admin/api/assignments/:sub", delete(admin::delete_assignment))
        .route("/admin/api/sessions", get(admin::list_sessions))
        .route("/admin/api/sessions/:id/shadow", post(admin::shadow_session))
        .route("/admin/api/sessions/:id", delete(admin::disconnect_session))

        // ── Internal: agent registration ──
        .route("/internal/register", post(registry::register_handler))
        .route("/internal/hosts/register", post(hosts::register_host))
        .route("/internal/hosts/:id/heartbeat", post(hosts::host_heartbeat))

        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.gateway_port);
    info!("streamio-gateway listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
