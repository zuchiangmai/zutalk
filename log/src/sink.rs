//! The file: whoever says something formats the line and offers it to a bounded queue, never
//! waiting; one thread, `log.writer`, appends it to `<dir>/<name>.jsonl` and turns the file at
//! its size. What the queue had no room for is counted, and said by the writer in a line of its
//! own before the next.

use crate::capture::{Book, Entered};
use crate::hear::{Hear, Out};
use crate::line::Line;
use crate::{Id, Level, error, info, warn};
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};
use tracing::{Dispatch, dispatcher};

/// The size a file turns at: PLAN.md §4.5.2.
pub const LOG_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// How many files are kept, the one written to included: PLAN.md §4.5.2.
pub const LOG_FILES_KEPT: usize = 5;
/// Lines waiting for the writer: a burst of some hundreds a second fits for seconds, in under
/// 1 MB. Full, a line is dropped and counted (`log.lost`).
pub const LOG_QUEUE_LINES: usize = 4096;
/// How long a flush, the guard's last one or the panic hook's, waits for the writer: long
/// enough for a full queue to reach a slow disk, short enough that quitting stays quick.
pub const LOG_FLUSH_WAIT_MS: u64 = 2000;

/// The thread that writes the file. Its owner is the `Guard`.
const WRITER_THREAD: &str = "log.writer";

/// How big the files get, how many are kept, how many lines may wait.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file_bytes: u64,
    pub files_kept: usize,
    pub queue_lines: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            file_bytes: LOG_FILE_BYTES,
            files_kept: LOG_FILES_KEPT,
            queue_lines: LOG_QUEUE_LINES,
        }
    }
}

/// Opens a file to append to: the port the writer writes through. A test gives one that holds
/// the writer back.
pub type Open = Arc<dyn Fn(&Path) -> io::Result<Box<dyn Write + Send>> + Send + Sync>;

fn append(path: &Path) -> io::Result<Box<dyn Write + Send>> {
    let file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    Ok(Box::new(file))
}

/// Whether a file of `written` bytes turns before a line of `line` bytes: when the line would
/// take it past `most`. A file with nothing in it takes any line, so no line turns forever.
pub fn must_turn(written: u64, line: u64, most: u64) -> bool {
    written > 0 && written.saturating_add(line) > most
}

enum Msg {
    Line(String),
    Stop,
}

/// What the senders and the writer share.
pub(crate) struct Sink {
    tx: SyncSender<Msg>,
    /// The number of the last line: a gap in a file is a line lost.
    seq: AtomicU64,
    /// Lines the queue had no room for.
    overflowed: AtomicU64,
    /// Lines the writer could not put in a file.
    unwritten: AtomicU64,
    refused: AtomicU64,
    /// Lines the queue took.
    sent: AtomicU64,
    /// After its guard is gone, nothing more is written.
    closed: AtomicBool,
    /// Lines the writer has handed to the file, as of its last flush.
    flushed: Mutex<u64>,
    turned: Condvar,
}

impl Sink {
    pub(crate) fn put(&self, line: &Line, refused: u64) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.refused.fetch_add(refused, Ordering::Relaxed);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        // Full: this line is lost, counted, and said by the writer before its next.
        match self.tx.try_send(Msg::Line(line.json(wall_ms(), seq))) {
            Ok(()) => self.sent.fetch_add(1, Ordering::Release),
            Err(_) => self.overflowed.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// Waits until what the queue took so far is in the file, at most `wait_ms`.
    fn flush(&self, wait_ms: u64) -> bool {
        let target = self.sent.load(Ordering::Acquire);
        let until = Instant::now() + Duration::from_millis(wait_ms);
        let mut flushed = lock(&self.flushed);
        while *flushed < target {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            flushed =
                self.turned.wait_timeout(flushed, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        true
    }

    fn lost(&self) -> u64 {
        self.overflowed.load(Ordering::Relaxed) + self.unwritten.load(Ordering::Relaxed)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wall_ms() -> u64 {
    SystemTime::UNIX_EPOCH.elapsed().map_or(0, |since| since.as_millis() as u64)
}

/// The writer's own state: the file, its size, and what it has said about losses.
struct Writer {
    dir: PathBuf,
    name: String,
    limits: Limits,
    open: Open,
    echo: bool,
    sink: Arc<Sink>,
    file: Option<BufWriter<Box<dyn Write + Send>>>,
    size: u64,
    /// Overflowed lines already said in a `log.lost`.
    said: u64,
    /// Lines taken from the queue.
    handled: u64,
    /// What the writer itself says goes here, not to the queue it empties.
    own: Arc<Book>,
}

/// The writer's own lines are few: one per turn or per burst of losses between two lines.
const OWN_KEEP_EVENTS: usize = 16;

impl Writer {
    fn path(&self, n: usize) -> PathBuf {
        match n {
            0 => self.dir.join(format!("{}.jsonl", self.name)),
            n => self.dir.join(format!("{}.{n}.jsonl", self.name)),
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        let own = Dispatch::new(Hear { level: Level::Debug, out: Out::Memory(self.own.clone()) });
        dispatcher::with_default(&own, || {
            loop {
                let msg = match rx.try_recv() {
                    Ok(msg) => msg,
                    Err(TryRecvError::Empty) => {
                        if !self.idle() {
                            continue;
                        }
                        match rx.recv() {
                            Ok(msg) => msg,
                            Err(_) => break,
                        }
                    }
                    Err(TryRecvError::Disconnected) => break,
                };
                match msg {
                    Msg::Line(text) => {
                        self.say_lost();
                        self.write(&text);
                        self.handled += 1;
                    }
                    Msg::Stop => break,
                }
            }
            self.idle();
        });
    }

    /// The queue is empty: what was written goes to the file. `false`: lines were lost while it
    /// did, so it is not idle after all and must say so first.
    fn idle(&mut self) -> bool {
        self.say_lost();
        if let Some(file) = self.file.as_mut()
            && file.flush().is_err()
        {
            self.file = None;
        }
        if self.sink.overflowed.load(Ordering::Relaxed) != self.said {
            return false;
        }
        *lock(&self.sink.flushed) = self.handled;
        self.sink.turned.notify_all();
        true
    }

    fn say_lost(&mut self) {
        let overflowed = self.sink.overflowed.load(Ordering::Relaxed);
        if overflowed > self.said {
            warn!("log.lost", n = overflowed - self.said);
            self.said = overflowed;
            self.write_own();
        }
    }

    /// The writer's own lines take no number: a gap in `seq` is only ever a line lost.
    fn write_own(&mut self) {
        for line in self.own.take() {
            self.append(&line.json(wall_ms(), 0));
        }
    }

    fn write(&mut self, text: &str) {
        if must_turn(self.size, text.len() as u64 + 1, self.limits.file_bytes) {
            self.turn();
        }
        self.append(text);
    }

    fn append(&mut self, text: &str) {
        if self.echo {
            let _ = writeln!(io::stderr(), "{text}"); // dropped: only a copy; the file is the log
        }
        if self.file.is_none() {
            let path = self.path(0);
            self.size = fs::metadata(&path).map_or(0, |meta| meta.len());
            self.file = (self.open)(&path).ok().map(BufWriter::new);
        }
        let Some(file) = self.file.as_mut() else {
            self.sink.unwritten.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if file.write_all(text.as_bytes()).and_then(|()| file.write_all(b"\n")).is_ok() {
            self.size += text.len() as u64 + 1;
        } else {
            self.file = None;
            self.sink.unwritten.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `name.jsonl` becomes `name.1.jsonl`, and so on down; the oldest goes.
    fn turn(&mut self) {
        if let Some(mut file) = self.file.take() {
            let _ = file.flush(); // dropped: what this flush cannot write, no later one could either
        }
        let kept = self.limits.files_kept.max(1);
        let _ = fs::remove_file(self.path(kept - 1)); // dropped: the oldest may not be there yet
        for n in (0..kept - 1).rev() {
            let _ = fs::rename(self.path(n), self.path(n + 1)); // dropped: a file not there yet has nothing to move
        }
        self.size = 0;
        let files = 1 + (1..kept).filter(|n| self.path(*n).exists()).count();
        info!("log.turned", files = files);
        self.write_own();
    }
}

/// Starts a writer on `dir`: the sink to hear into, and the guard that owns the thread.
fn start(
    dir: &Path,
    name: &str,
    limits: Limits,
    echo: bool,
    open: Open,
) -> io::Result<(Arc<Sink>, Guard)> {
    fs::create_dir_all(dir)?;
    let (tx, rx) = sync_channel(limits.queue_lines);
    let sink = Arc::new(Sink {
        tx,
        seq: AtomicU64::new(0),
        overflowed: AtomicU64::new(0),
        unwritten: AtomicU64::new(0),
        refused: AtomicU64::new(0),
        sent: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        flushed: Mutex::new(0),
        turned: Condvar::new(),
    });
    let writer = Writer {
        dir: dir.into(),
        name: name.into(),
        limits,
        open,
        echo,
        sink: sink.clone(),
        file: None,
        size: 0,
        said: 0,
        handled: 0,
        own: Arc::new(Book::new(OWN_KEEP_EVENTS)),
    };
    let thread = thread::Builder::new().name(WRITER_THREAD.into()).spawn(move || writer.run(rx))?;
    Ok((sink.clone(), Guard { sink, thread: Some(thread) }))
}

/// Owns the writer. Dropped, it writes what is left (waiting at most `LOG_FLUSH_WAIT_MS`),
/// stops the writer, and nothing said after reaches the file.
pub struct Guard {
    sink: Arc<Sink>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let flushed = self.sink.flush(LOG_FLUSH_WAIT_MS);
        self.sink.closed.store(true, Ordering::Release);
        let stopped = self.sink.tx.try_send(Msg::Stop).is_ok();
        // A writer that stands still past the wait is left behind rather than waited for.
        if flushed
            && stopped
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join(); // dropped: a writer that panicked has nothing left to write
        }
    }
}

/// A file log not installed for the process: a test enters it on its thread.
pub struct Scope(Dispatch);

impl Scope {
    /// This thread hears into the file until the guard is dropped.
    pub fn enter(&self) -> Entered {
        Entered::of(&self.0)
    }
}

/// The same file log as `install`'s, not installed: `scope.enter()` makes it this thread's.
pub fn files(dir: &Path, name: &str, level: Level, limits: Limits) -> io::Result<(Scope, Guard)> {
    files_with(dir, name, level, limits, Arc::new(append))
}

/// `files`, through another port to open each file with.
pub fn files_with(
    dir: &Path,
    name: &str,
    level: Level,
    limits: Limits,
    open: Open,
) -> io::Result<(Scope, Guard)> {
    let (sink, guard) = start(dir, name, limits, false, open)?;
    Ok((Scope(Dispatch::new(Hear { level, out: Out::File(sink) })), guard))
}

/// The process's one log, once installed.
static INSTALLED: OnceLock<Arc<Sink>> = OnceLock::new();

/// The process's one log: `<dir>/<name>.jsonl`, at `level`, each line also on standard error
/// if `echo`. Every thread hears into it unless it has a receiver of its own. A panic is said
/// (`app.panic`) and flushed before the hook that was there runs. A second call is an error.
pub fn install(dir: &Path, name: &str, level: Level, echo: bool) -> io::Result<Guard> {
    let installed = || io::Error::new(io::ErrorKind::AlreadyExists, "the log is installed");
    if INSTALLED.get().is_some() {
        return Err(installed());
    }
    let (sink, guard) = start(dir, name, Limits::default(), echo, Arc::new(append))?;
    let dispatch = Dispatch::new(Hear { level, out: Out::File(sink.clone()) });
    dispatcher::set_global_default(dispatch).map_err(|_| installed())?;
    INSTALLED.set(sink).map_err(|_| installed())?;
    hook();
    Ok(guard)
}

/// Waits until the installed log has put in its file all said so far, at most `wait_ms`.
/// `true` if it has, or if none is installed.
pub fn flush(wait_ms: u64) -> bool {
    INSTALLED.get().is_none_or(|sink| sink.flush(wait_ms))
}

/// Lines the installed log dropped: no room in its queue, or no file to write to.
pub fn lost() -> u64 {
    INSTALLED.get().map_or(0, |sink| sink.lost())
}

/// Fields the installed log turned away.
pub fn refused() -> u64 {
    INSTALLED.get().map_or(0, |sink| sink.refused.load(Ordering::Relaxed))
}

/// Says a panic: the thread, the source file and the line. Never the message, which may have
/// something said formatted into it.
fn hook() {
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let current = thread::current();
        let (file, line) = info.location().map_or(("", 0), |at| (at.file(), at.line()));
        let file = source(file);
        error!(
            "app.panic",
            thread = Id(current.name().unwrap_or("unnamed")),
            file = Id(&file),
            line = line
        );
        let _ = flush(LOG_FLUSH_WAIT_MS); // dropped: the panic goes on, said or not
        before(info);
    }));
}

/// A source path a line may hold: this workspace's are relative already; another crate's is
/// absolute, through the home folder and so a user's name, and keeps its last two parts.
fn source(file: &str) -> String {
    let file = file.replace('\\', "/");
    let absolute = file.starts_with('/') || file.as_bytes().get(1) == Some(&b':');
    if !absolute {
        return file;
    }
    let parts: Vec<&str> = file.rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}
