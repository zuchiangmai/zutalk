//! The receiver under every way of hearing: the file, a capture, the writer's own lines. It
//! takes events of ours at its level and nothing else: no span, no other library's event (it
//! may hold an address or a sentence).

use crate::capture::Book;
use crate::line::{Fields, Line};
use crate::sink::Sink;
use crate::{BRAND, Level, is_code};
use std::sync::Arc;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

/// Where what is heard goes.
pub(crate) enum Out {
    File(Arc<Sink>),
    Memory(Arc<Book>),
}

pub(crate) struct Hear {
    pub level: Level,
    pub out: Out,
}

/// The code of an event our macros made; `None` for anything else.
pub(crate) fn ours(meta: &Metadata<'static>) -> Option<&'static str> {
    let name: &'static str = meta.name();
    name.strip_prefix(BRAND).filter(|code| meta.is_event() && is_code(code))
}

/// The one span id handed out: spans are not kept, so every one is the same nothing.
const NO_SPAN: u64 = 1;

impl Subscriber for Hear {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        // Asked each time, not cached: receivers at other levels hear on other threads.
        if ours(meta).is_some() { Interest::sometimes() } else { Interest::never() }
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        meta.is_event()
            && meta.name().strip_prefix(BRAND).is_some_and(is_code)
            && self.level.keeps(Level::of(meta.level()))
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(NO_SPAN)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let Some(code) = ours(meta) else { return };
        let level = Level::of(meta.level());
        if !self.level.keeps(level) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let line = Line { level, component: meta.target(), code, fields: fields.list };
        match &self.out {
            Out::File(sink) => sink.put(&line, fields.refused),
            Out::Memory(book) => book.put(line, fields.refused),
        }
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}
