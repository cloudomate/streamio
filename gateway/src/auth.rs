use crate::{session::COOKIE_NAME, AppState};
use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, SameSite};
use openidconnect::{
    core::{CoreClient, CoreProviderMetadata, CoreResponseType},
    reqwest::async_http_client,
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use redis::AsyncCommands;
use serde::Deserialize;
use streamio_types::Role;
use tracing::{error, info};

const PKCE_TTL: u64 = 600; // 10 minutes

pub struct OidcClient {
    client: CoreClient,
    redirect_uri: String,
}

impl OidcClient {
    pub async fn discover(
        issuer: String,
        client_id: String,
        client_secret: String,
        redirect_uri: String,
    ) -> Result<Self> {
        let provider_metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer)?,
            async_http_client,
        )
        .await?;

        let client = CoreClient::from_provider_metadata(
            provider_metadata,
            ClientId::new(client_id),
            Some(ClientSecret::new(client_secret)),
        )
        .set_redirect_uri(RedirectUrl::new(redirect_uri.clone())?);

        Ok(OidcClient { client, redirect_uri })
    }
}

/// GET /auth/login — redirect user to OIDC provider
pub async fn login_handler(State(state): State<AppState>) -> Response {
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf_token, nonce) = state
        .oidc
        .client
        .authorize_url(
            AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("openid".into()))
        .add_scope(Scope::new("email".into()))
        .add_scope(Scope::new("profile".into()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    // Store PKCE verifier + nonce in Redis keyed by CSRF token (10-minute TTL)
    // Format: "<pkce_verifier>:<nonce>"
    let value = format!("{}:{}", pkce_verifier.secret(), nonce.secret());
    let key = format!("pkce:{}", csrf_token.secret());
    let mut redis = state.redis.clone();
    if let Err(e) = redis
        .set_ex::<_, _, ()>(&key, value.as_str(), PKCE_TTL)
        .await
    {
        error!("Redis write error storing PKCE verifier: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    Redirect::to(auth_url.as_str()).into_response()
}

#[derive(Deserialize)]
pub struct CallbackParams {
    code: String,
    state: String, // CSRF token
}

/// GET /auth/callback — exchange code, issue session cookie
pub async fn callback_handler(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
) -> Response {
    // Retrieve PKCE verifier + nonce from Redis
    let key = format!("pkce:{}", params.state);
    let mut redis = state.redis.clone();
    let stored: Option<String> = match redis.get_del(&key).await {
        Ok(v) => v,
        Err(e) => {
            error!("Redis read error for PKCE verifier: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let stored = match stored {
        Some(v) => v,
        None => {
            return (StatusCode::BAD_REQUEST, "Invalid or expired login session").into_response()
        }
    };

    // Split stored value into verifier and nonce
    let (verifier_secret, nonce_secret) = match stored.split_once(':') {
        Some((v, n)) => (v.to_owned(), n.to_owned()),
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Malformed session data").into_response()
        }
    };

    let pkce_verifier = PkceCodeVerifier::new(verifier_secret);
    let stored_nonce = Nonce::new(nonce_secret);

    // Exchange the authorization code for tokens
    let token_response = match state
        .oidc
        .client
        .exchange_code(AuthorizationCode::new(params.code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(async_http_client)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            error!("Token exchange error: {e}");
            return (StatusCode::UNAUTHORIZED, "OIDC token exchange failed").into_response();
        }
    };

    // Extract user info from ID token claims
    let id_token = match token_response.id_token() {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "No ID token in response").into_response(),
    };

    let claims = match id_token.claims(&state.oidc.client.id_token_verifier(), &stored_nonce) {
        Ok(c) => c,
        Err(e) => {
            error!("ID token claim verification error: {e}");
            return (StatusCode::UNAUTHORIZED, "ID token verification failed").into_response();
        }
    };

    let sub = claims.subject().to_string();
    let email = claims
        .email()
        .map(|e| e.to_string())
        .unwrap_or_else(|| sub.clone());

    // Determine role: check if sub is in ADMIN_SUBS
    let role = if state.config.admin_subs.contains(&sub) {
        Role::Admin
    } else {
        Role::User
    };

    // Look up existing backend assignment (if any)
    let backend_id = state.registry.get_assignment(&sub).await;

    info!("User logged in: sub={sub} email={email} role={role:?}");

    // Issue internal JWT
    let token = match state.session.issue(sub, email, role, backend_id) {
        Ok(t) => t,
        Err(e) => {
            error!("JWT issue error: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let cookie = Cookie::build((COOKIE_NAME, token))
        .http_only(true)
        .same_site(SameSite::Lax) // Lax allows redirect from OIDC provider
        .path("/")
        .build();

    (
        axum_extra::extract::cookie::CookieJar::new().add(cookie),
        Redirect::to("/"),
    )
        .into_response()
}

/// GET /auth/logout — clear session cookie
pub async fn logout_handler() -> Response {
    // Set cookie with immediate expiry to clear it from the browser
    let cookie = Cookie::build((COOKIE_NAME, ""))
        .http_only(true)
        .path("/")
        .max_age(axum_extra::extract::cookie::time::Duration::ZERO)
        .build();

    (
        axum_extra::extract::cookie::CookieJar::new().add(cookie),
        Redirect::to("/auth/login"),
    )
        .into_response()
}
