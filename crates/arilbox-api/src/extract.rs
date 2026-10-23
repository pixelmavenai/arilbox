//! Extractors that reject with the standard error body instead of axum's plain text.

use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

fn invalid(message: String) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", message)
}

/// A JSON body. Malformed JSON, wrong types and unknown fields → `400 invalid_request`.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|_| invalid("Could not read the request body".into()))?;
        serde_json::from_slice(&bytes)
            .map(ApiJson)
            .map_err(|e| invalid(format!("Invalid request body: {e}")))
    }
}

/// Like [`ApiJson`], but an empty body means `T::default()`.
pub struct OptionalJson<T>(pub T);

impl<S, T> FromRequest<S> for OptionalJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Default,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|_| invalid("Could not read the request body".into()))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(OptionalJson(T::default()));
        }
        serde_json::from_slice(&bytes)
            .map(OptionalJson)
            .map_err(|e| invalid(format!("Invalid request body: {e}")))
    }
}

/// Parses the request's query string with the standard error on failure.
pub fn query<T: DeserializeOwned>(uri: &axum::http::Uri) -> Result<T, ApiError> {
    axum::extract::Query::<T>::try_from_uri(uri)
        .map(|q| q.0)
        .map_err(|e| invalid(format!("Invalid query string: {}", e.body_text())))
}
