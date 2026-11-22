//! Authentication, request bodies and error responses.

use crate::Services;
use axum::Json;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use control_plane_common::{Caller, Error};
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// The cookie of a browser session (D103).
pub const SESSION_COOKIE: &str = "sona_session";

/// The caller, resolved from `Authorization: Bearer <credential>` (D85) or from the
/// session cookie of the UI (D103).
pub struct Authenticated(pub Caller);

impl FromRequestParts<Arc<Services>> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        services: &Arc<Services>,
    ) -> Result<Self, Self::Rejection> {
        let bearer = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if let Some(credential) = bearer {
            return Ok(Self(services.credentials.authenticate(credential).await?));
        }
        let session = session_cookie(&parts.headers).ok_or(Error::Unauthenticated)?;
        // A browser sends the cookie with any request to us, a forged one included.
        // SameSite=Strict stops other sites; checking Origin on every change makes
        // sure the page that asks is ours.
        if !parts.method.is_safe() && !same_origin(&parts.headers) {
            return Err(
                Error::Forbidden("the request does not come from this site".to_owned()).into(),
            );
        }
        Ok(Self(services.credentials.session_caller(session).await?))
    }
}

pub fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
}

/// Whether `Origin` names the host the request was sent to.
fn same_origin(headers: &HeaderMap) -> bool {
    let host = headers.get(HOST).and_then(|v| v.to_str().ok());
    let origin = headers
        .get(ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once("://"))
        .map(|(_, host)| host);
    matches!((host, origin), (Some(host), Some(origin)) if host == origin)
}

/// A JSON body whose rejection is an [`Error`] like every other failure.
pub struct Body<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|rejection| Error::InvalidInput(rejection.body_text()))?;
        Ok(Self(value))
    }
}

/// An [`Error`] as an HTTP response (D85).
pub struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::Unauthenticated => StatusCode::UNAUTHORIZED,
            Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let error = match self.0 {
            // Internal details may name files or queries: they go to the log only.
            Error::Internal(detail) => {
                tracing::error!(%detail, "internal error");
                Error::Internal("internal error".to_owned())
            }
            other => other,
        };
        (status, Json(error)).into_response()
    }
}
