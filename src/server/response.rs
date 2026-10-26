use axum::{http::StatusCode, Json};
use serde::Serialize;

/// Common JSON format for all REST endpoints.
#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub data: T,
    pub status: u16,
}

/// Return matching HTTP and JSON status codes.
pub fn respond<T: Serialize>(
    status: StatusCode,
    data: T,
) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        status,
        Json(ApiResponse {
            data,
            status: status.as_u16(),
        }),
    )
}
