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

/// Simple base64 decode (standard alphabet, with padding).
fn base64_decode(input: &str) -> Result<Vec<u8>, ()> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &b in input.as_bytes() {
        if b == b'=' || b == b'\n' || b == b'\r' {
            continue;
        }
        let val = TABLE.iter().position(|&c| c == b).ok_or(())?;
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(output)
}

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

    // Determine role: check if sub or any group ID is in ADMIN_SUBS
    // Entra ID includes group object IDs in the "groups" claim of the ID token.
    // We decode the raw JWT to extract groups without needing extra API calls.
    let groups: Vec<String> = {
        let raw_jwt = id_token.to_string();
        let parts: Vec<&str> = raw_jwt.split('.').collect();
        if parts.len() >= 2 {
            // Decode the payload (base64url)
            use openidconnect::core::CoreJsonWebKey;
            let payload = parts[1];
            // Pad base64url to base64
            let padded = match payload.len() % 4 {
                2 => format!("{}==", payload),
                3 => format!("{}=", payload),
                _ => payload.to_string(),
            };
            let decoded = padded.replace('-', "+").replace('_', "/");
            match openidconnect::url::Url::parse("data:;base64,") {
                _ => {
                    // Use standard base64 decoding
                    if let Ok(bytes) = base64_decode(&decoded) {
                        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            val.get("groups")
                                .and_then(|g| g.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default()
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    }
                }
            }
        } else {
            vec![]
        }
    };

    info!("Login: sub={sub}, email={email}, groups={groups:?}, admin_subs={:?}", state.config.admin_subs);

    let is_admin = state.config.admin_subs.contains(&sub)
        || groups.iter().any(|g| state.config.admin_subs.contains(g));
    let role = if is_admin { Role::Admin } else { Role::User };

    // Track user in known_users table (for admin assignment dropdown)
    let _ = sqlx::query(
        "INSERT INTO known_users (sub, email, last_login) VALUES ($1, $2, now())
         ON CONFLICT (sub) DO UPDATE SET email = $2, last_login = now()",
    )
    .bind(&sub)
    .bind(&email)
    .execute(&state.db)
    .await;

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

    let is_https = state.config.gateway_origin.starts_with("https");
    let mut cookie_builder = Cookie::build((COOKIE_NAME, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/");
    if is_https {
        cookie_builder = cookie_builder.secure(true);
    }
    let cookie = cookie_builder.build();

    (
        axum_extra::extract::cookie::CookieJar::new().add(cookie),
        Redirect::to("/portal"),
    )
        .into_response()
}

/// GET /auth/logout — clear session cookie
pub async fn logout_handler(State(state): State<AppState>) -> Response {
    let is_https = state.config.gateway_origin.starts_with("https");
    let mut cookie_builder = Cookie::build((COOKIE_NAME, ""))
        .http_only(true)
        .path("/")
        .max_age(cookie::time::Duration::ZERO);
    if is_https {
        cookie_builder = cookie_builder.secure(true);
    }
    let cookie = cookie_builder
        .build();

    (
        axum_extra::extract::cookie::CookieJar::new().add(cookie),
        Redirect::to("/auth/login"),
    )
        .into_response()
}
