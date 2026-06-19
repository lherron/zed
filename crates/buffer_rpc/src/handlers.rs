use gpui::App;
use release_channel::{AppVersion, ReleaseChannel};

use crate::protocol::{
    InitializeParams, InitializeResult, PROTOCOL_VERSION, PingResult, Request, Response,
    same_protocol_major,
};

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub fn handle_request(request: Request, cx: &mut App) -> Option<Response> {
    if request.jsonrpc != crate::protocol::JSONRPC_VERSION {
        return Some(Response::error(
            request.id,
            INVALID_REQUEST,
            "invalid JSON-RPC version",
        ));
    }

    let id = request.id;
    match request.method.as_str() {
        "initialize" => Some(initialize(id, request.params, cx)),
        "ping" => Some(Response::result(id, PingResult { ok: true })),
        _ => Some(Response::error(id, METHOD_NOT_FOUND, "method not found")),
    }
}

fn initialize(
    id: Option<serde_json::Value>,
    params: Option<serde_json::Value>,
    cx: &mut App,
) -> Response {
    let params = match params {
        Some(params) => match serde_json::from_value::<InitializeParams>(params) {
            Ok(params) => params,
            Err(_) => return Response::error(id, INVALID_PARAMS, "invalid initialize params"),
        },
        None => InitializeParams {
            client_name: None,
            protocol_version: None,
        },
    };

    if !same_protocol_major(params.protocol_version.as_deref()) {
        return Response::error(id, INVALID_PARAMS, "unsupported protocol major version");
    }

    let result = InitializeResult {
        server_version: AppVersion::global(cx).to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        pid: std::process::id(),
        release_channel: ReleaseChannel::try_global(cx)
            .unwrap_or_default()
            .dev_name()
            .to_string(),
    };

    Response::result(id, result)
}
