mod admin;
mod auth;
mod middleware;
mod provisioner;
mod proxy;
mod registry;
mod session;

use anyhow::Result;
use axum::{
    routing::{delete, get, post},
    Router,
};
use provisioner::{DefaultVmSpec, KubeVirtProvisioner, VmProvisioner};
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
    pub provisioner: Option<Arc<dyn VmProvisioner>>,
    pub default_vm: Option<Arc<DefaultVmSpec>>,
    pub config: Arc<Config>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub gateway_port: u16,
    pub gateway_origin: String,
    pub jwt_secret: String,
    pub admin_subs: Vec<String>,
    // KubeVirt provisioner settings
    pub kubevirt_enabled: bool,
    pub kubevirt_ns: String,
    pub kubevirt_gateway_url: String,
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
            kubevirt_enabled: std::env::var("KUBEVIRT_ENABLED")
                .unwrap_or_else(|_| "false".into())
                .eq_ignore_ascii_case("true"),
            kubevirt_ns: std::env::var("KUBEVIRT_NAMESPACE")
                .unwrap_or_else(|_| "vdi".into()),
            kubevirt_gateway_url: std::env::var("KUBEVIRT_GATEWAY_URL")
                .unwrap_or_else(|_| String::new()),
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

    // Run migrations (inline SQL)
    sqlx::query(include_str!("../migrations/001_init.sql"))
        .execute(&db)
        .await?;
    sqlx::query(include_str!("../migrations/002_vm_columns.sql"))
        .execute(&db)
        .await?;

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

    // Backend registry
    let registry = Arc::new(registry::BackendRegistry::new(db.clone()));

    // KubeVirt provisioner (optional)
    let (provisioner, default_vm): (Option<Arc<dyn VmProvisioner>>, Option<Arc<DefaultVmSpec>>) =
        if config.kubevirt_enabled {
            let gateway_url = if config.kubevirt_gateway_url.is_empty() {
                config.gateway_origin.clone()
            } else {
                config.kubevirt_gateway_url.clone()
            };
            let p = KubeVirtProvisioner::new(
                config.kubevirt_ns.clone(),
                gateway_url,
                config.jwt_secret.clone(),
            )
            .await?;
            info!("KubeVirt provisioner enabled (namespace={})", config.kubevirt_ns);
            let default_spec = DefaultVmSpec::from_env();
            if default_spec.is_none() {
                tracing::warn!(
                    "KUBEVIRT_ENABLED=true but DEFAULT_BASE_PVC is not set — auto-provisioning on first login is disabled"
                );
            }
            (Some(Arc::new(p)), default_spec.map(Arc::new))
        } else {
            (None, None)
        };

    let state = AppState {
        db,
        redis,
        session,
        oidc,
        registry: registry.clone(),
        provisioner,
        default_vm,
        config: config.clone(),
    };

    // Background health-poll task (every 30s)
    tokio::spawn(registry::health_poll_task(registry, state.db.clone()));

    let app = Router::new()
        // Public: auth
        .route("/auth/login", get(auth::login_handler))
        .route("/auth/callback", get(auth::callback_handler))
        .route("/auth/logout", get(auth::logout_handler))
        // Public: health
        .route("/healthz", get(|| async { "ok" }))
        // Authenticated: stream + admin UI
        .route("/", get(proxy::index_handler))
        .route("/ws", get(proxy::ws_handler))
        .route("/admin", get(admin::admin_ui_handler))
        // Admin REST API — backends
        .route("/admin/api/backends", get(admin::list_backends))
        .route("/admin/api/backends/provision", post(admin::provision_backend))
        // Admin REST API — VM lifecycle
        .route("/admin/api/vms/:id/start", post(admin::vm_start))
        .route("/admin/api/vms/:id/stop", post(admin::vm_stop))
        .route("/admin/api/vms/:id", delete(admin::vm_delete))
        .route("/admin/api/vms/:id/state", get(admin::vm_state))
        // Admin REST API — users & sessions
        .route("/admin/api/users", get(admin::list_users))
        .route("/admin/api/assignments", post(admin::create_assignment))
        .route("/admin/api/assignments/:sub", delete(admin::delete_assignment))
        .route("/admin/api/sessions", get(admin::list_sessions))
        .route("/admin/api/sessions/:id/shadow", post(admin::shadow_session))
        .route("/admin/api/sessions/:id", delete(admin::disconnect_session))
        // Internal: backend self-registration
        .route("/internal/register", post(registry::register_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.gateway_port);
    info!("streamio-gateway listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
