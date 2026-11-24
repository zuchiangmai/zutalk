//! The transport port for a live caption lane: one WebSocket connection to the realtime service,
//! abstracted so tests can stand in an in-memory duplex service without real sockets.

use crate::rt::supervise::End;
use futures_util::{Sink, Stream};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error, Message};

/// A boxed, `Send` future returned by `Dial`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One connection to the realtime service, as a lane uses it.
pub trait Socket:
    Stream<Item = Result<Message, Error>> + Sink<Message, Error = Error> + Send + Unpin
{
}

impl<T> Socket for T where
    T: Stream<Item = Result<Message, Error>> + Sink<Message, Error = Error> + Send + Unpin
{
}

/// An opened realtime connection behind a trait object.
pub type Wire = Box<dyn Socket>;

/// Opens one connection to `endpoint`. The lane wraps calls in its own connect timeout.
pub type Dial = Arc<dyn Fn(&str) -> BoxFuture<'static, Result<Wire, End>> + Send + Sync>;

/// The real transport port: connects over WebSocket (`tokio_tungstenite::connect_async`).
pub fn dial() -> Dial {
    Arc::new(|endpoint: &str| {
        let endpoint = endpoint.to_owned();
        Box::pin(async move {
            let (ws, _) =
                connect_async(endpoint.as_str()).await.map_err(|_| End::Retry("connect"))?;
            Ok(Box::new(ws) as Wire)
        })
    })
}
