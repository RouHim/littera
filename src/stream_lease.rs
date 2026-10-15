//! The model's compute lease: one session plus the stream that borrows it.
//!
//! `transcribe_cpp::Session::stream` returns a `Stream<'_>` borrowing the
//! session, and a dictation needs both for its whole life. The borrow is
//! extended to `'static` in exactly one function in this module; everything
//! that could break the resulting invariant is confined here:
//!
//! * `_session` and `stream` are private, so no other module can reach the
//!   session while the stream is alive (the aliasing the extension depends on),
//! * `stream` is declared before `_session`, so it is dropped first and the
//!   extended borrow can never outlive its referent,
//! * `session` is boxed, so its address is stable even when the lease moves.
//!
//! The surface is only what a dictation does — feed, read, finalize — so no
//! caller can observe the layout or rely on it.

use std::thread;
use std::time::Duration;

use transcribe_cpp::{
    Error as AsrError, Model, RunOptions, Session, Stream, StreamOptions, StreamUpdate,
};

/// `Busy` can only come from transient native bookkeeping in this single-worker
/// design; retry a few times instead of failing the dictation.
const BUSY_RETRIES: u32 = 5;
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(20);

/// A session plus the stream that borrows it.
pub(crate) struct StreamLease {
    /// Declared first on purpose: fields drop in declaration order, so the
    /// stream releases the model's compute lease before its session goes away.
    stream: Stream<'static>,
    /// Boxed so the stream's borrow survives moving the lease. Nothing reads it
    /// — it is held so the box's address (and thus the extended borrow) stays
    /// valid — but it must be declared *after* `stream` and is therefore only
    /// dropped once the stream is gone.
    _session: Box<Session>,
}

impl StreamLease {
    /// Open a session on `model` and begin its stream.
    pub(crate) fn open(
        model: &Model,
        run: &RunOptions,
        options: &StreamOptions,
    ) -> Result<Self, String> {
        let mut session = Box::new(model.session().map_err(|err| transcription_failed(&err))?);
        let stream = begin_stream(session.as_mut(), run, options)?;
        Ok(Self {
            stream,
            _session: session,
        })
    }

    /// Feed 16 kHz mono PCM; `true` means the text changed and is worth reading.
    pub(crate) fn feed(&mut self, samples: &[f32]) -> Result<bool, String> {
        let update: StreamUpdate = self
            .stream
            .feed(samples)
            .map_err(|err| transcription_failed(&err))?;
        Ok(update.result_changed)
    }

    /// The current display text (committed plus tentative parts).
    pub(crate) fn display(&self) -> String {
        self.stream.text().display()
    }

    /// Flush the stream; the text is read afterwards.
    pub(crate) fn finalize(&mut self) -> Result<(), String> {
        self.stream
            .finalize()
            .map(|_| ())
            .map_err(|err| transcription_failed(&err))
    }
}

/// Begin a stream on `session`, retrying briefly while the library reports the
/// model busy.
///
/// This is the only place where the session borrow is extended: the caller owns
/// a live boxed `Session`, so its address does not change, and the returned
/// stream is dropped by [`StreamLease`] before that session is.
fn begin_stream(
    session: &mut Session,
    run: &RunOptions,
    options: &StreamOptions,
) -> Result<Stream<'static>, String> {
    // SAFETY: `session` is a live box at a stable address; the borrow is kept
    // inside this module (private fields, drop order) and never handed out.
    let session: &'static mut Session = unsafe { &mut *std::ptr::from_mut(session) };
    let mut attempt = 0;
    loop {
        // The reborrow of `session` lasts only as long as the value below.
        let failure = match session.stream(run, options) {
            Ok(stream) => {
                // SAFETY: as documented above — the boxed session outlives the
                // stream, and `StreamLease` drops the stream first.
                return Ok(unsafe { std::mem::transmute::<Stream<'_>, Stream<'static>>(stream) });
            }
            Err(err) => err,
        };
        if !matches!(&failure, AsrError::Busy(_)) || attempt >= BUSY_RETRIES {
            return Err(transcription_failed(&failure));
        }
        attempt += 1;
        tracing::error!(
            error = %failure,
            attempt,
            "the model reported Busy despite the dictation queue; retrying"
        );
        thread::sleep(BUSY_RETRY_DELAY);
    }
}

/// The user-facing message for a failed session or stream call.
pub(crate) fn transcription_failed(err: &AsrError) -> String {
    format!("transcription failed: {err}")
}
