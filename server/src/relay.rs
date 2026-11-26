//! login.md § The Relay: the sealed key of a login between devices, from the login page to the
//! receiving page. One message, no owner, in memory only, for five minutes.

use crate::{Server, lock, now};
use axum::extract::{Path, State, rejection::BytesRejection};
use axum::http::{Method, StatusCode, header};
use axum::{body::Bytes, response::IntoResponse, response::Response};
use std::sync::Arc;

/// The largest message.
pub const MESSAGE: usize = 64 << 10;

/// `GET` and `POST /.well-known/zu-relay/{room}`, open to every origin: the login page is
/// usually in another. A `POST` of bytes is not a simple request: browsers ask with `OPTIONS`.
pub async fn relay(
    State(server): State<Arc<Server>>,
    method: Method,
    Path(room): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let ((held, life), now) = (server.config.relay, now());
    let mut rooms = lock(&server.relay);
    rooms.retain(|_, (born, _)| now < *born + life);
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    let answer = match (method, body, rooms.get(&room)) {
        _ if room.len() != 32 || !room.bytes().all(hex) => Err(StatusCode::NOT_FOUND),
        (Method::OPTIONS, ..) => Err(StatusCode::NO_CONTENT),
        (Method::POST, Err(_), _) => Err(StatusCode::PAYLOAD_TOO_LARGE),
        (Method::POST, _, Some(_)) => Err(StatusCode::CONFLICT),
        (Method::POST, _, None) if rooms.len() >= held => Err(StatusCode::SERVICE_UNAVAILABLE),
        (Method::POST, Ok(body), None) => {
            rooms.insert(room, (now, body));
            Ok(None)
        }
        (Method::GET, _, Some((_, message))) => Ok(Some(message.clone())),
        (Method::GET, _, None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::METHOD_NOT_ALLOWED),
    };
    let cors = [
        (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST"),
        (header::ACCESS_CONTROL_ALLOW_HEADERS, "content-type"),
    ];
    match answer {
        Ok(Some(message)) => {
            let bytes = [(header::CONTENT_TYPE, "application/octet-stream")];
            (cors, bytes, message).into_response()
        }
        Ok(None) => (cors, StatusCode::NO_CONTENT).into_response(),
        Err(status) => (cors, status).into_response(),
    }
}
