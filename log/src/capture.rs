//! Hearing in a test, and taking the hearer along: a capture is this thread's receiver while it
//! lives; a task or a thread started with `carry` or `carrier` hears where its starter heard.
//! Two tests side by side each hear only their own.

use crate::Level;
use crate::hear::{Hear, Out};
use crate::line::Line;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tracing::Dispatch;
use tracing::dispatcher::{self, DefaultGuard};
use tracing::instrument::WithSubscriber;

/// The most events a capture keeps; past it the oldest go, counted. A test that says more than
/// this has a loop that says too much; 64 Ki lines of about 200 bytes is some 13 MB at most.
pub const CAPTURE_KEEP_EVENTS: usize = 65_536;

/// Events held in memory, at most `keep`.
pub(crate) struct Book {
    keep: usize,
    held: Mutex<Held>,
}

#[derive(Default)]
struct Held {
    lines: VecDeque<Line>,
    lost: u64,
    refused: u64,
}

impl Book {
    pub(crate) fn new(keep: usize) -> Book {
        Book { keep, held: Mutex::default() }
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn put(&self, line: Line, refused: u64) {
        let mut held = self.held();
        held.refused += refused;
        if held.lines.len() >= self.keep {
            held.lines.pop_front();
            held.lost += 1;
        }
        held.lines.push_back(line);
    }

    /// What it holds, emptied: the writer's own lines.
    pub(crate) fn take(&self) -> Vec<Line> {
        self.held().lines.drain(..).collect()
    }
}

/// What this thread, and whatever it carried the log to, said while the capture lived.
pub struct Seen {
    book: Arc<Book>,
    _entered: DefaultGuard,
}

/// A capture, this thread's receiver until the `Seen` is dropped.
pub fn capture(level: Level) -> Seen {
    let book = Arc::new(Book::new(CAPTURE_KEEP_EVENTS));
    let dispatch = Dispatch::new(Hear { level, out: Out::Memory(book.clone()) });
    Seen { book, _entered: dispatcher::set_default(&dispatch) }
}

impl Seen {
    /// Every event, in the order said.
    pub fn all(&self) -> Vec<Line> {
        self.book.held().lines.iter().cloned().collect()
    }

    /// The events of one code, in the order said.
    pub fn of(&self, code: &str) -> Vec<Line> {
        self.book.held().lines.iter().filter(|line| line.code == code).cloned().collect()
    }

    /// How many events of one code.
    pub fn count(&self, code: &str) -> usize {
        self.book.held().lines.iter().filter(|line| line.code == code).count()
    }

    /// Fields turned away: text that was not plain, names a line's head has.
    pub fn refused(&self) -> u64 {
        self.book.held().refused
    }

    /// Events let go past `CAPTURE_KEEP_EVENTS`, oldest first.
    pub fn lost(&self) -> u64 {
        self.book.held().lost
    }
}

/// A task that tells whoever its starter told.
pub fn carry<F: Future>(work: F) -> impl Future<Output = F::Output> {
    work.with_current_subscriber()
}

/// This thread's receiver, to be entered on another thread or in a blocking call.
pub fn carrier() -> Carrier {
    Carrier(dispatcher::get_default(Dispatch::clone))
}

/// A receiver taken along.
#[derive(Clone)]
pub struct Carrier(Dispatch);

impl Carrier {
    /// This thread hears where the carrier was made, until the guard is dropped.
    pub fn enter(&self) -> Entered {
        Entered::of(&self.0)
    }
}

/// A receiver entered on this thread: dropped, the one before is back.
pub struct Entered {
    _guard: DefaultGuard,
}

impl Entered {
    pub(crate) fn of(dispatch: &Dispatch) -> Entered {
        Entered { _guard: dispatcher::set_default(dispatch) }
    }
}
