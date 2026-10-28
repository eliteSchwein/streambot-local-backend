use crate::abstracts::api::{Api, ApiFuture};
use serde_json::Value;

pub struct StatusApi;
impl Api for StatusApi {
    fn rest_endpoint(&self) -> Option<&str> { Some("status") }
    fn websocket_method(&self) -> Option<&str> { Some("status") }
    fn handle(&self, _data: Value) -> ApiFuture<'_> { Box::pin(async { crate::status() }) }
}

pub fn register_all() -> Result<(), String> {
    super::apis::register(StatusApi)
}
