//! One API definition for both REST and JSON-RPC WebSocket requests.
use std::{future::Future, pin::Pin};
use serde_json::{json, Value};

pub type ApiFuture<'a> = Pin<Box<dyn Future<Output = Value> + Send + 'a>>;

/// A file uploaded through a multipart/form-data REST POST.
/// `data` contains the binary bytes; filenames are never used as filesystem paths.
#[derive(Debug, Clone)]
pub struct UploadedFile {
    pub field_name: String,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// Mirrors the endpoint settings of the TypeScript BaseApi.
/// The REST and WebSocket servers own connections and routing.
pub trait Api: Send + Sync + 'static {
    fn rest_endpoint(&self) -> Option<&str> { None }
    fn rest_post(&self) -> bool { false }
    fn websocket_method(&self) -> Option<&str> { None }

    /// Shared JSON handler for REST and WebSocket.
    fn handle(&self, data: Value) -> ApiFuture<'_>;

    /// Override on APIs that accept multipart file uploads. `data` holds the
    /// other multipart form fields, and `files` holds actual file bytes.
    /// Normal JSON POSTs continue to use `handle`.
    fn handle_upload(&self, _data: Value, _files: Vec<UploadedFile>) -> ApiFuture<'_> {
        Box::pin(async { json!({"error": "This API does not support file uploads"}) })
    }
}
