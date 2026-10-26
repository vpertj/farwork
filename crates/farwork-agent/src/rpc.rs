//! Minimal JSON-RPC 2.0-shaped codec for codex app-server.
//!
//! codex speaks newline-delimited JSON over stdio and does not put a "jsonrpc"
//! field on the wire (see /tmp/codex-schema/JSONRPCRequest.json et al.), so this
//! module keeps the envelope shape it actually uses: optional id, method+params
//! for requests/notifications, id+result or id+error for responses.

use serde_json::{json, Value};

pub type RpcId = Value;

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RpcMessage {
    /// Server -> client request (an approval prompt, for example).
    Request {
        id: RpcId,
        method: String,
        params: Value,
    },
    /// Either direction; carries no id and expects no reply.
    Notification { method: String, params: Value },
    /// Successful reply to one of our requests.
    Response { id: RpcId, result: Value },
    /// Error reply to one of our requests.
    Failure { id: RpcId, error: RpcError },
}

impl RpcMessage {
    /// Classify one decoded JSON value.
    ///
    /// Returns None for anything that is not a JSON-RPC frame; the caller drops
    /// it (a stray log line on stdout must not kill the session).
    pub fn decode(value: Value) -> Option<Self> {
        let object = value.as_object()?;
        let id = object.get("id").filter(|value| !value.is_null()).cloned();
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (method, id) {
            (Some(method), Some(id)) => Some(Self::Request {
                id,
                method,
                params: object.get("params").cloned().unwrap_or(Value::Null),
            }),
            (Some(method), None) => Some(Self::Notification {
                method,
                params: object.get("params").cloned().unwrap_or(Value::Null),
            }),
            (None, Some(id)) => {
                if let Some(result) = object.get("result") {
                    return Some(Self::Response {
                        id,
                        result: result.clone(),
                    });
                }
                let error = object.get("error")?.as_object()?;
                Some(Self::Failure {
                    id,
                    error: RpcError {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_owned(),
                        data: error.get("data").cloned(),
                    },
                })
            }
            (None, None) => None,
        }
    }
}

pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"id": id, "method": method, "params": params})
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({"method": method, "params": params})
}

pub fn result(id: &RpcId, result: Value) -> Value {
    json!({"id": id, "result": result})
}

pub fn error(id: &RpcId, code: i64, message: &str) -> Value {
    json!({"id": id, "error": {"code": code, "message": message}})
}

/// Frame one message as a newline-delimited line.
pub fn encode_line(value: &Value) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_encodes_without_jsonrpc_field() {
        let value = request(7, "thread/start", json!({"cwd": "/tmp"}));
        assert_eq!(
            value,
            json!({"id": 7, "method": "thread/start", "params": {"cwd": "/tmp"}})
        );
        assert!(value.get("jsonrpc").is_none());
    }

    #[test]
    fn line_codec_adds_exactly_one_trailing_newline() {
        let line = encode_line(&json!({"id": 1, "result": {}})).unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        assert!(!line.trim_end().ends_with('\n'));
    }

    #[test]
    fn decodes_server_request_with_integer_id() {
        let msg = RpcMessage::decode(json!({
            "id": 42,
            "method": "item/commandExecution/requestApproval",
            "params": {"itemId": "i1"}
        }))
        .unwrap();
        match msg {
            RpcMessage::Request { id, method, params } => {
                assert_eq!(id, json!(42));
                assert_eq!(method, "item/commandExecution/requestApproval");
                assert_eq!(params["itemId"], json!("i1"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decodes_notification_with_extra_emitted_at_field() {
        let msg = RpcMessage::decode(json!({
            "method": "turn/started",
            "params": {"threadId": "t1"},
            "emittedAtMs": 1792984101
        }))
        .unwrap();
        match msg {
            RpcMessage::Notification { method, params } => {
                assert_eq!(method, "turn/started");
                assert_eq!(params["threadId"], json!("t1"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decodes_success_and_error_responses() {
        let ok = RpcMessage::decode(json!({"id": 3, "result": {"turn": {"id": "x"}}})).unwrap();
        assert_eq!(
            ok,
            RpcMessage::Response {
                id: json!(3),
                result: json!({"turn": {"id": "x"}})
            }
        );
        let bad = RpcMessage::decode(json!({
            "id": "req_1",
            "error": {"code": -32601, "message": "nope", "data": {"hint": 1}}
        }))
        .unwrap();
        assert_eq!(
            bad,
            RpcMessage::Failure {
                id: json!("req_1"),
                error: RpcError {
                    code: -32601,
                    message: String::from("nope"),
                    data: Some(json!({"hint": 1}))
                }
            }
        );
    }

    #[test]
    fn null_id_is_treated_as_a_notification() {
        let msg = RpcMessage::decode(json!({"id": null, "method": "initialized"})).unwrap();
        assert!(matches!(msg, RpcMessage::Notification { .. }));
    }

    #[test]
    fn non_rpc_values_decode_to_none() {
        assert_eq!(RpcMessage::decode(json!("just a string")), None);
        assert_eq!(RpcMessage::decode(json!({"hello": "world"})), None);
        assert_eq!(RpcMessage::decode(json!([1, 2, 3])), None);
    }

    #[test]
    fn responses_round_trip_through_the_codec() {
        let value = result(&json!(9), json!({"thread": {"id": "t1"}}));
        assert_eq!(
            RpcMessage::decode(value).unwrap(),
            RpcMessage::Response {
                id: json!(9),
                result: json!({"thread": {"id": "t1"}})
            }
        );
        let value = error(&json!(9), -32601, "unsupported");
        match RpcMessage::decode(value).unwrap() {
            RpcMessage::Failure { id, error } => {
                assert_eq!(id, json!(9));
                assert_eq!(error.code, -32601);
                assert_eq!(error.message, "unsupported");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
