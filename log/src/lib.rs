//! The one way ZuTalk says what happened: an event is a stable code, then flat fields that are
//! numbers, booleans, codes written in the source, or ids the app made itself. Nothing a person
//! said, typed or is called can be put in one: its type does not fit (`docs/EVENTS.md`,
//! `docs/build/phases/P1.md` §3.13.1–§3.13.2).
//!
//! ```
//! use zutalk_log::{Id, info};
//! info!("lane.link", lane = 0u8, from = "Connecting", to = "Live", why = "");
//! info!("recording.start", rec = Id("19a3f0c2b7e00d4c1f2"), lanes = 2u8);
//! ```
//!
//! Lines go to a file that turns at its size, written by a thread of its own: whoever says
//! something never waits for the disk. A test hears only what it, and whatever it carries the
//! log to, says (`capture`, `carry`, `carrier`).
//!
//! Three things do not compile. A field that is a `String`:
//!
//! ```compile_fail
//! let title = String::from("a title");
//! zutalk_log::info!("talk.change", title = title);
//! ```
//! ```
//! let title = String::from("19a3f0c2");
//! zutalk_log::info!("talk.change", title = zutalk_log::Id(&title));
//! ```
//!
//! A field that is a borrowed `&str`, which could hold anything:
//!
//! ```compile_fail
//! fn said(words: &str) {
//!     zutalk_log::info!("lane.said", words = words);
//! }
//! ```
//! ```
//! fn said(words: &'static str) {
//!     zutalk_log::info!("lane.said", words = words);
//! }
//! ```
//!
//! A code that is not lower-case words joined by dots:
//!
//! ```compile_fail
//! zutalk_log::info!("Lane Reopen", n = 1);
//! ```
//! ```
//! zutalk_log::info!("lane.reopen", n = 1);
//! ```

mod capture;
mod hear;
mod line;
mod sink;

pub use capture::{CAPTURE_KEEP_EVENTS, Carrier, Entered, Seen, capture, carrier, carry};
pub use line::{Line, Value};
pub use sink::{
    Guard, LOG_FILE_BYTES, LOG_FILES_KEPT, LOG_FLUSH_WAIT_MS, LOG_QUEUE_LINES, Limits, Open, Scope,
    files, files_with, flush, install, lost, must_turn, refused,
};

/// The most bytes of any text in a line, a code's included: room for an id or a code, none for
/// a sentence.
pub const TEXT_MOST_BYTES: usize = 64;

/// What our macros put before an event's code in the name of its callsite (the literal in
/// `__event!`): how the receivers tell an event of ours, whose fields the compiler checked, from
/// any other library's.
pub(crate) const BRAND: &str = "zutalk:";

/// How much is said: `error` is what makes a person lose something; `warn`, what put itself
/// right; `info`, a change of state, a command, a job; `debug`, what happens every frame, block
/// or second, off unless asked for (`PLAN.md` §4.5.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    /// As a line says it.
    pub fn code(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }

    /// The level a setting such as `ZUTALK_LOG` asks for: `debug`, or else `info`.
    pub fn from_setting(setting: Option<&str>) -> Level {
        if setting == Some("debug") { Level::Debug } else { Level::Info }
    }

    /// Whether a receiver at this level keeps an event at `level`.
    pub(crate) fn keeps(self, level: Level) -> bool {
        level as u8 <= self as u8
    }

    pub(crate) fn of(level: &tracing::Level) -> Level {
        match *level {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            _ => Level::Debug,
        }
    }
}

/// An event's code: two or more parts joined by dots, each a lower-case letter and then
/// lower-case letters, digits or `_`; at most `TEXT_MOST_BYTES`. Checked when a macro compiles.
pub const fn is_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    if bytes.is_empty() || bytes.len() > TEXT_MOST_BYTES {
        return false;
    }
    let (mut at, mut parts, mut starts) = (0, 1, true);
    while at < bytes.len() {
        let byte = bytes[at];
        if starts {
            if !byte.is_ascii_lowercase() {
                return false;
            }
            starts = false;
        } else if byte == b'.' {
            parts += 1;
            starts = true;
        } else if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
            return false;
        }
        at += 1;
    }
    !starts && parts >= 2
}

/// Text a line may hold: `[A-Za-z0-9._:/-]`, at most `TEXT_MOST_BYTES`, maybe none. No space,
/// no letter outside ASCII: no sentence fits.
pub fn is_plain(text: &str) -> bool {
    text.len() <= TEXT_MOST_BYTES
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
}

/// An id the app made itself: a recording's, a window's label. Not checked when made (it may be
/// on a hot path); checked when written, and turned away there if it is not plain.
#[derive(Clone, Copy)]
pub struct Id<'a>(pub &'a str);

mod sealed {
    pub trait Sealed {}
}

/// What a field may be: a number, a boolean, a code written in the source, or an `Id`. Nothing
/// a person said, typed or is called has one of these types, and no other type can have it.
pub trait Plain: sealed::Sealed {
    #[doc(hidden)]
    type Out: tracing::Value;
    #[doc(hidden)]
    fn out(&self) -> Self::Out;
}

macro_rules! plain_as {
    ($($kind:ty => $out:ty),*) => {$(
        impl sealed::Sealed for $kind {}
        impl Plain for $kind {
            type Out = $out;
            fn out(&self) -> $out {
                <$out>::from(*self)
            }
        }
    )*};
}

plain_as!(u8 => u64, u16 => u64, u32 => u64, u64 => u64, i32 => i64, i64 => i64, f64 => f64, bool => bool);

impl sealed::Sealed for usize {}
impl Plain for usize {
    type Out = u64;
    fn out(&self) -> u64 {
        *self as u64
    }
}

impl sealed::Sealed for &'static str {}
impl Plain for &'static str {
    type Out = &'static str;
    fn out(&self) -> &'static str {
        self
    }
}

impl sealed::Sealed for Id<'_> {}
impl<'a> Plain for Id<'a> {
    type Out = &'a str;
    fn out(&self) -> &'a str {
        self.0
    }
}

#[doc(hidden)]
pub mod __private {
    pub use tracing;

    /// Every field's value passes through here: what is not `Plain` does not compile.
    pub fn plain<P: crate::Plain>(value: &P) -> P::Out {
        value.out()
    }
}

/// What makes a person lose something.
#[macro_export]
macro_rules! error {
    ($code:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::__event!(ERROR, $code $(, $name = $value)*)
    };
}

/// What went wrong and put itself right.
#[macro_export]
macro_rules! warn {
    ($code:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::__event!(WARN, $code $(, $name = $value)*)
    };
}

/// A change of state, a command, a job.
///     info!("lane.link", lane = 0u8, from = "Connecting", to = "Live", why = "");
#[macro_export]
macro_rules! info {
    ($code:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::__event!(INFO, $code $(, $name = $value)*)
    };
}

/// What happens every frame, block or second: off unless asked for.
#[macro_export]
macro_rules! debug {
    ($code:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::__event!(DEBUG, $code $(, $name = $value)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __event {
    ($level:ident, $code:literal $(, $name:ident = $value:expr)*) => {{
        const _: () = ::core::assert!(
            $crate::is_code($code),
            "an event's code is two or more lower-case words joined by dots"
        );
        $crate::__private::tracing::event!(
            name: ::core::concat!("zutalk:", $code),
            target: ::core::module_path!(),
            $crate::__private::tracing::Level::$level,
            { $($name = $crate::__private::plain(&$value)),* }
        )
    }};
}
