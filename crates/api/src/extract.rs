//! Authentication, request bodies and error responses.

use crate::Services;
use axum::Json;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use control_plane_common::{Caller, Error};
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// The caller, resolved from `Authorization: Bearer <credential>` (D85).
pub struct Authenticated(pub Caller);

impl FromRequestParts<Arc<Services>> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        services: &Arc<Services>,
    ) -> Result<Self, Self::Rejection> {
        let credential = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(Error::Unauthenticated)?;
        Ok(Self(services.credentials.authenticate(credential).await?))
    }
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
