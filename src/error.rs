use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: serde_json::Value,
}
impl RpcError {
    pub fn new(code: i32, kind: &str, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
            data: serde_json::json!({"kind": kind}),
        }
    }
    pub fn params(message: impl ToString) -> Self {
        Self::new(-32602, "INVALID_PARAMS", message)
    }
    pub fn missing() -> Self {
        Self::new(-32004, "OBJECT_NOT_FOUND", "object does not exist")
    }
    pub fn busy(message: impl ToString) -> Self {
        Self::new(-32001, "BUSY", message)
    }
}
pub type RpcResult = Result<serde_json::Value, RpcError>;
pub const INVALID: i64 = -1;
pub const LIMIT: i64 = -2;
pub const TIMEOUT: i64 = -3;
pub const DENIED: i64 = -4;
pub const HOST_ERROR: i64 = -5;
pub const CLOSED: i64 = -6;
