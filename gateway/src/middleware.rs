use crate::{session::COOKIE_NAME, AppState};
use axum::{
    async_trait,
    extract::{FromRequestParts, State},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::CookieJar;
use streamio_types::{Role, SessionClaims};

/// Axum extractor: requires a valid session cookie.
/// On success, provides `SessionClaims`. On failure, redirects to /auth/login.
pub struct RequireSession(pub SessionClaims);

#[async_trait]
impl FromRequestParts<AppState> for RequireSession {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let jar = CookieJar::from_request_parts(parts, state)
            .await
            .map_err(|e| e.into_response())?;

        let token = jar
            .get(COOKIE_NAME)
            .map(|c| c.value().to_owned())
            .ok_or_else(|| Redirect::to("/auth/login").into_response())?;

        let claims = state
            .session
            .verify(&token)
            .map_err(|_| Redirect::to("/auth/login").into_response())?;

        Ok(RequireSession(claims))
    }
}

/// Axum extractor: requires Admin role. Returns 403 if user is not an admin.
pub struct RequireAdmin(pub SessionClaims);

#[async_trait]
impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let RequireSession(claims) = RequireSession::from_request_parts(parts, state).await?;

        if claims.role != Role::Admin {
            return Err((StatusCode::FORBIDDEN, "Admin access required").into_response());
        }

        Ok(RequireAdmin(claims))
    }
}
