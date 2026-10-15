//! Microphone capture, down-mixing, resampling and 100 ms chunking.
//!
//! [`start`] opens the default input device on a dedicated capture thread — a [`cpal::Stream`]
//! is not `Sync`, so it never leaves that thread. The stream callback only copies its raw buffer
//! into the capture thread; down-mixing, resampling to [`TARGET_RATE`], chunking and the `rms`
//! calculation all happen there, and a single stop message tears the whole thing down.
//!
//! [`chunks`] is the same chunker for audio that is already 16 kHz mono (the `--audio-file`
//! verification path) and [`read_wav_16k_mono`] loads such a file from disk.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvError, Sender};
use std::thread::JoinHandle;

use anyhow::{Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Data, Error as CpalError, InputCallbackInfo, SampleFormat, Stream};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{FixedSync, Indexing, Resampler};

/// Sample rate the engine consumes.
pub const TARGET_RATE: u32 = 16_000;
/// Chunk length handed to the engine: 100 ms at [`TARGET_RATE`].
pub const CHUNK_SAMPLES: usize = 1_600;
/// Shortest recording that is worth transcribing (0.3 s at [`TARGET_RATE`]).
pub const MIN_RECORDING_SAMPLES: u64 = 4_800;
/// Longest recording before the state machine stops it (5 minutes).
pub const MAX_RECORDING_SECS: u64 = 300;

/// The one device-opening failure a user can act on.
const NO_INPUT_DEVICE: &str =
    "no default audio input device found (PipeWire/ALSA): check `wpctl status`";

/// Block size used to stream a WAV `data` chunk; a multiple of every supported frame size.
const WAV_BLOCK_BYTES: usize = 64 * 1024;
/// Largest `fmt` chunk body read into memory (the standard body is 16 to 40 bytes).
const WAV_FMT_MAX: u64 = 64;

/// One 100 ms block of 16 kHz mono audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub samples: Vec<f32>,
    pub rms: f32,
}

/// Loudness of `samples` in `0.0..=1.0`, used for the overlay level bar.
fn rms_of(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|value| value * value).sum();
    (sum / samples.len() as f32).sqrt().clamp(0.0, 1.0)
}

/// Split 16 kHz mono audio into 100 ms chunks with their `rms`.
///
/// The final chunk keeps whatever is left over: it is never padded and never dropped.
pub fn chunks(samples: &[f32]) -> Vec<Chunk> {
    let mut out = Vec::new();
    for part in samples.chunks(CHUNK_SAMPLES) {
        let chunk = Chunk {
            samples: part.to_vec(),
            rms: rms_of(part),
        };
        out.push(chunk);
    }
    out
}

/// Decode device samples to `f32` in `-1.0..=1.0`.
///
/// The buffer is exactly the layout the input configuration announced, in native byte order
/// (which is what `cpal` hands to a raw stream callback). The scaling matches `dasp_sample`,
/// which is what a typed `cpal` stream would have used.
fn decode_into(format: SampleFormat, bytes: &[u8], out: &mut Vec<f32>) {
    out.clear();
    out.reserve(bytes.len() / 2);
    match format {
        SampleFormat::F32 => {
            for raw in bytes.as_chunks::<4>().0 {
                out.push(f32::from_ne_bytes(*raw));
            }
        }
        SampleFormat::I16 => {
            for raw in bytes.as_chunks::<2>().0 {
                let value = i16::from_ne_bytes(*raw);
                out.push(value as f32 / 32_768.0);
            }
        }
        SampleFormat::U16 => {
            for raw in bytes.as_chunks::<2>().0 {
                let value = u16::from_ne_bytes(*raw);
                out.push((value as f32 - 32_768.0) / 32_768.0);
            }
        }
        SampleFormat::I32 => {
            for raw in bytes.as_chunks::<4>().0 {
                let value = i32::from_ne_bytes(*raw);
                out.push(value as f32 / 2_147_483_648.0);
            }
        }
        // Unreachable: the format is checked before the stream is built. Handled anyway, so a
        // device that changes format mid-stream cannot feed garbage to the engine.
        other => {
            tracing::error!(format = %other, "unsupported sample format in the audio callback");
        }
    }
}

/// Average the channels of interleaved audio into mono. A trailing partial frame is ignored.
fn downmix_into(interleaved: &[f32], channels: u16, out: &mut Vec<f32>) {
    out.clear();
    let channels = usize::from(channels.max(1));
    if channels == 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    for frame in interleaved.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        out.push(sum / channels as f32);
    }
}

/// Wrap mono audio in the one-channel adapter `rubato` reads from.
fn mono_input(buf: &[f32]) -> Result<InterleavedSlice<&[f32]>> {
    match InterleavedSlice::new(buf, 1, buf.len()) {
        Ok(adapter) => Ok(adapter),
        Err(err) => bail!("cannot hand audio to the resampler: {err}"),
    }
}

/// Wrap a mono output buffer in the one-channel adapter `rubato` writes to.
fn mono_output(buf: &mut [f32]) -> Result<InterleavedSlice<&mut [f32]>> {
    let frames = buf.len();
    match InterleavedSlice::new_mut(buf, 1, frames) {
        Ok(adapter) => Ok(adapter),
        Err(err) => bail!("cannot hand audio to the resampler: {err}"),
    }
}

/// `rubato` FFT resampler for one channel with fixed chunk sizes.
///
/// The startup delay is dropped, so the output is aligned with the input, and the tail is
/// pumped out when recording stops, so no real audio is held back.
struct ResampleStage {
    inner: rubato::Fft<f32>,
    chunk_in: usize,
    in_acc: Vec<f32>,
    in_scratch: Vec<f32>,
    out_scratch: Vec<f32>,
    delay_left: usize,
    /// Input frames fed so far, and output frames emitted so far (after the delay drop).
    fed: u64,
    emitted: usize,
}

impl ResampleStage {
    fn new(rate_in: u32) -> Result<Self> {
        let built = rubato::Fft::<f32>::new(
            rate_in as usize,
            TARGET_RATE as usize,
            CHUNK_SAMPLES,
            1,
            FixedSync::Both,
        );
        let inner = match built {
            Ok(inner) => inner,
            Err(err) => bail!("cannot resample from {rate_in} Hz to {TARGET_RATE} Hz: {err}"),
        };
        let chunk_in = inner.input_frames_next();
        let chunk_out = inner.output_frames_next();
        let delay_left = inner.output_delay();
        tracing::debug!(
            rate_in,
            chunk_in,
            chunk_out,
            delay_left,
            "audio resampler ready"
        );
        Ok(Self {
            chunk_in,
            in_acc: Vec::with_capacity(chunk_in * 2),
            in_scratch: vec![0.0; chunk_in],
            out_scratch: vec![0.0; chunk_out],
            delay_left,
            fed: 0,
            emitted: 0,
            inner,
        })
    }

    /// Feed input-rate mono samples, appending resampled 16 kHz samples to `out`.
    fn push(&mut self, mono: &[f32], out: &mut Vec<f32>) -> Result<()> {
        self.fed += mono.len() as u64;
        self.in_acc.extend_from_slice(mono);
        while self.in_acc.len() >= self.chunk_in {
            self.in_scratch
                .copy_from_slice(&self.in_acc[..self.chunk_in]);
            self.in_acc.copy_within(self.chunk_in.., 0);
            let len = self.in_acc.len();
            self.in_acc.truncate(len - self.chunk_in);
            self.process(None, out)?;
        }
        Ok(())
    }

    /// Resample whatever is buffered, then pump silence through the resampler until every fed
    /// input frame has come out: the output lags the input by the resampler's delay, so without
    /// this the last milliseconds of a recording would be cut off.
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let real = self.in_acc.len();
        if real > 0 {
            self.in_scratch[..real].copy_from_slice(&self.in_acc);
            self.in_scratch[real..].fill(0.0);
            self.in_acc.clear();
            let partial = Indexing {
                partial_len: Some(real),
                ..Indexing::default()
            };
            self.process(Some(&partial), out)?;
        }
        let wanted = (self.fed as f64 * self.inner.resample_ratio()).round() as usize;
        let silence = Indexing {
            partial_len: Some(0),
            ..Indexing::default()
        };
        while self.emitted < wanted {
            let before = out.len();
            self.process(Some(&silence), out)?;
            if out.len() == before {
                tracing::warn!("the resampler stopped producing output early");
                break;
            }
        }
        if self.emitted > wanted {
            // The pump overshot: drop the extra padding, never real audio.
            let extra = (self.emitted - wanted).min(out.len());
            out.truncate(out.len() - extra);
            self.emitted = wanted;
        }
        Ok(())
    }

    fn process(&mut self, indexing: Option<&Indexing>, out: &mut Vec<f32>) -> Result<()> {
        let input = mono_input(&self.in_scratch)?;
        let mut output = mono_output(&mut self.out_scratch)?;
        let call = self
            .inner
            .process_into_buffer(&input, &mut output, indexing);
        let produced = match call {
            Ok((_, produced)) => produced,
            Err(err) => bail!("resampling to {TARGET_RATE} Hz failed: {err}"),
        };
        let mut resampled = &self.out_scratch[..produced];
        if self.delay_left > 0 {
            let skipped = self.delay_left.min(resampled.len());
            self.delay_left -= skipped;
            resampled = &resampled[skipped..];
        }
        self.emitted += resampled.len();
        out.extend_from_slice(resampled);
        Ok(())
    }
}

/// Turns device buffers into 16 kHz mono [`Chunk`]s.
///
/// Owned by the capture thread: the audio callback only hands raw bytes over, so nothing locks
/// on the audio thread.
struct Pipeline {
    format: SampleFormat,
    channels: u16,
    resampler: Option<ResampleStage>,
    decoded: Vec<f32>,
    mono: Vec<f32>,
    pending: Vec<f32>,
    tx: Sender<Chunk>,
}

impl Pipeline {
    fn new(format: SampleFormat, channels: u16, rate_in: u32, tx: Sender<Chunk>) -> Result<Self> {
        let supported = matches!(
            format,
            SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16 | SampleFormat::I32
        );
        if !supported {
            let detail = "expected f32, i16, u16 or i32";
            tracing::error!(format = %format, detail, "unsupported audio sample format");
            bail!("the device uses the unsupported sample format {format} ({detail})");
        }
        let resampler = if rate_in == TARGET_RATE {
            None
        } else {
            Some(ResampleStage::new(rate_in)?)
        };
        Ok(Self {
            format,
            channels: channels.max(1),
            resampler,
            decoded: Vec::new(),
            mono: Vec::new(),
            pending: Vec::with_capacity(CHUNK_SAMPLES * 2),
            tx,
        })
    }

    /// Consume one device buffer and emit every complete 100 ms chunk it completes.
    fn push_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        decode_into(self.format, bytes, &mut self.decoded);
        downmix_into(&self.decoded, self.channels, &mut self.mono);
        if let Some(resampler) = &mut self.resampler {
            resampler.push(&self.mono, &mut self.pending)?;
        } else {
            self.pending.extend_from_slice(&self.mono);
        }
        self.emit_full_chunks();
        Ok(())
    }

    /// Emit the trailing partial chunk and flush the resampler. Called when recording stops.
    fn flush(&mut self) -> Result<()> {
        if let Some(resampler) = &mut self.resampler {
            resampler.flush(&mut self.pending)?;
        }
        self.emit_full_chunks();
        if !self.pending.is_empty() {
            let samples = std::mem::take(&mut self.pending);
            self.send_chunk(samples);
        }
        Ok(())
    }

    fn emit_full_chunks(&mut self) {
        while self.pending.len() >= CHUNK_SAMPLES {
            let samples: Vec<f32> = self.pending.drain(..CHUNK_SAMPLES).collect();
            self.send_chunk(samples);
        }
    }

    fn send_chunk(&mut self, samples: Vec<f32>) {
        let rms = rms_of(&samples);
        if self.tx.send(Chunk { samples, rms }).is_err() {
            tracing::trace!("dropping audio chunk: the receiver is gone");
        }
    }
}

/// What the capture thread receives: audio from the stream callback, a fatal stream error, or
/// the stop request from [`Capture::stop`].
enum CaptureMsg {
    Samples(Vec<u8>),
    StreamError(String),
    Stop,
}

/// The capture thread's startup report: the device rate and channel count, or why it failed.
type StartupReport = std::result::Result<(u32, u16), String>;

/// A playing input stream plus everything needed to turn its audio into chunks.
type CaptureSetup = (Stream, Pipeline, u32, u16);

/// A device-opening failure, reported as a string that is safe to show to the user.
type SetupResult = std::result::Result<CaptureSetup, String>;

/// A running microphone capture. Owns the capture thread and, inside it, the `cpal::Stream`.
pub struct Capture {
    rate_in: u32,
    channels: u16,
    control: Option<Sender<CaptureMsg>>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Sample rate the device delivers, before resampling.
    pub fn rate_in(&self) -> u32 {
        self.rate_in
    }

    /// Channel count the device delivers, before down-mixing.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Stop capturing and drop the stream. Idempotent.
    pub fn stop(&mut self) {
        let Some(control) = self.control.take() else {
            return;
        };
        if control.send(CaptureMsg::Stop).is_err() {
            tracing::debug!("the audio capture thread had already stopped");
        }
        let thread = self.thread.take();
        if thread.is_some_and(|handle| handle.join().is_err()) {
            tracing::warn!("the audio capture thread panicked");
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start capturing the default input device, sending 100 ms 16 kHz mono chunks to `tx`.
pub fn start(tx: Sender<Chunk>) -> Result<Capture> {
    let (control_tx, control_rx) = std::sync::mpsc::channel::<CaptureMsg>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<StartupReport>();
    let builder = std::thread::Builder::new();
    let builder = builder.name("lingua-capture".to_owned());
    // The stream callbacks send through their own handle; `Capture` keeps the other one.
    let callback_tx = control_tx.clone();
    let body = move || capture_thread(control_rx, callback_tx, ready_tx, tx);
    let spawned = builder.spawn(body);
    let thread = match spawned {
        Ok(thread) => thread,
        Err(err) => bail!("cannot spawn the audio capture thread: {err}"),
    };

    match ready_rx.recv() {
        Ok(Ok((rate_in, channels))) => {
            tracing::info!(rate = rate_in, channels, "audio capture started");
            Ok(Capture {
                rate_in,
                channels,
                control: Some(control_tx),
                thread: Some(thread),
            })
        }
        Ok(Err(message)) => {
            if thread.join().is_err() {
                tracing::warn!("the audio capture thread panicked");
            }
            bail!("{message}");
        }
        Err(RecvError) => {
            let panicked = thread.join().is_err();
            let what = if panicked { "panicked" } else { "exited" };
            bail!("the audio capture thread {what} before opening the input device");
        }
    }
}

/// Opens the default input device, plays the stream until it is dropped, and pumps its audio
/// through the [`Pipeline`]. Everything that is not `Sync` stays on this thread.
fn capture_thread(
    control: Receiver<CaptureMsg>,
    callback_tx: Sender<CaptureMsg>,
    ready: Sender<StartupReport>,
    tx: Sender<Chunk>,
) {
    let setup = build_capture(callback_tx, tx);
    let (stream, mut pipeline, rate_in, channels) = match setup {
        Ok(setup) => setup,
        Err(message) => {
            ready.send(Err(message)).ok();
            return;
        }
    };
    if ready.send(Ok((rate_in, channels))).is_err() {
        // The caller went away before the stream was handed over.
        return;
    }

    let mut failure: Option<anyhow::Error> = None;
    loop {
        match control.recv() {
            Ok(CaptureMsg::Samples(bytes)) => {
                if let Err(err) = pipeline.push_bytes(&bytes) {
                    failure = Some(err);
                    break;
                }
            }
            Ok(CaptureMsg::StreamError(message)) => {
                tracing::debug!(error = %message, "stopping capture after a stream error");
                break;
            }
            Ok(CaptureMsg::Stop) => break,
            Err(RecvError) => break,
        }
    }

    if failure.is_none() {
        failure = pipeline.flush().err();
    }
    if let Some(err) = failure {
        tracing::error!(error = %err, "audio capture stopped early");
    }
    // Dropping the stream stops the device and joins the backend's audio thread.
    drop(stream);
}

/// Open the default input stream. `callback_tx` goes to the stream callbacks; errors are the
/// user-visible strings reported through the ready channel.
fn build_capture(callback_tx: Sender<CaptureMsg>, tx: Sender<Chunk>) -> SetupResult {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return Err(NO_INPUT_DEVICE.to_owned());
    };
    let supported = match device.default_input_config() {
        Ok(supported) => supported,
        Err(err) => {
            return Err(format!(
                "cannot query the default input configuration: {err}"
            ));
        }
    };

    let rate_in = supported.sample_rate();
    let channels = supported.channels();
    let format = supported.sample_format();
    let pipeline = match Pipeline::new(format, channels, rate_in, tx) {
        Ok(pipeline) => pipeline,
        Err(err) => return Err(format!("{err:#}")),
    };

    let audio_control = callback_tx.clone();
    let data_callback = move |data: &Data, _info: &InputCallbackInfo| {
        let bytes = data.bytes().to_vec();
        if audio_control.send(CaptureMsg::Samples(bytes)).is_err() {
            tracing::trace!("dropping captured audio: the capture thread is gone");
        }
    };

    let error_control = callback_tx;
    let mut error_logged = false;
    let error_callback = move |err: CpalError| {
        // Once per stream: a broken device repeats the same error in every callback.
        if !error_logged {
            error_logged = true;
            tracing::warn!(error = %err, "audio capture stream error");
        }
        if fatal_stream_error(err.kind()) {
            let message = err.to_string();
            error_control.send(CaptureMsg::StreamError(message)).ok();
        }
    };

    let built = device.build_input_stream_raw(
        supported.config(),
        format,
        data_callback,
        error_callback,
        None,
    );
    let stream = match built {
        Ok(stream) => stream,
        Err(err) => return Err(format!("cannot open the default input device: {err}")),
    };
    let started = stream.play();
    if let Err(err) = started {
        return Err(format!("cannot start audio capture: {err}"));
    }
    Ok((stream, pipeline, rate_in, channels))
}

/// Whether a stream error means the stream is finished rather than a momentary hiccup.
fn fatal_stream_error(kind: cpal::ErrorKind) -> bool {
    !matches!(
        kind,
        cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged
    )
}

/// WAVE encodings the verification path accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WavEncoding {
    Pcm16,
    F32,
}

impl WavEncoding {
    const fn sample_bytes(self) -> usize {
        match self {
            Self::Pcm16 => 2,
            Self::F32 => 4,
        }
    }

    fn decode(self, bytes: &[u8], out: &mut Vec<f32>) {
        match self {
            Self::Pcm16 => {
                for raw in bytes.as_chunks::<2>().0 {
                    let value = i16::from_le_bytes(*raw);
                    out.push(value as f32 / 32_768.0);
                }
            }
            Self::F32 => {
                for raw in bytes.as_chunks::<4>().0 {
                    out.push(f32::from_le_bytes(*raw));
                }
            }
        }
    }
}

/// Name the file in an I/O error.
fn io_error(path: &Path, err: std::io::Error) -> anyhow::Error {
    anyhow!("{}: {err}", path.display())
}

/// The requirement every rejected WAV is measured against.
fn wav_requirement(path: &Path, rate: u32, channels: u16) -> String {
    let name = path.display();
    format!("{name} must be 16 kHz mono PCM WAV (got {rate} Hz, {channels} channels)")
}

/// Read a 16 kHz mono PCM16 or float32 WAV into samples in `-1.0..=1.0`.
///
/// Only the `--audio-file` verification path uses this; the microphone path never touches it.
pub fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let file = File::open(path).map_err(|err| io_error(path, err))?;
    let mut reader = BufReader::new(file);

    let mut riff = [0u8; 12];
    let header = reader.read_exact(&mut riff);
    header.map_err(|err| io_error(path, err))?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        bail!("{} is not a RIFF/WAVE file", path.display());
    }

    let mut encoding: Option<WavEncoding> = None;
    let mut samples: Option<Vec<f32>> = None;
    loop {
        let mut chunk_header = [0u8; 8];
        let head = reader.read_exact(&mut chunk_header);
        match head {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(io_error(path, err)),
        }
        let id = [
            chunk_header[0],
            chunk_header[1],
            chunk_header[2],
            chunk_header[3],
        ];
        let raw_len = [
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ];
        let len = u64::from(u32::from_le_bytes(raw_len));
        let pad = len & 1;

        match &id {
            b"fmt " => {
                let take = len.min(WAV_FMT_MAX) as usize;
                let mut body = vec![0u8; take];
                let filled = read_full(&mut reader, &mut body);
                let filled = filled.map_err(|err| io_error(path, err))?;
                if filled < take {
                    bail!("{} has a truncated fmt chunk", path.display());
                }
                let parsed = parse_fmt(path, &body)?;
                // Skip whatever is left of the body plus the RIFF pad byte.
                let rest = len - take as u64 + pad;
                let skipped = skip_bytes(&mut reader, rest);
                skipped.map_err(|err| io_error(path, err))?;
                encoding = Some(parsed);
            }
            b"data" => {
                let Some(parsed) = encoding else {
                    bail!("{} has a data chunk before its fmt chunk", path.display());
                };
                let data = read_data(path, &mut reader, len, parsed)?;
                samples = Some(data);
                break;
            }
            _ => {
                let want = len + pad;
                let skipped = skip_bytes(&mut reader, want);
                let skipped = skipped.map_err(|err| io_error(path, err))?;
                if skipped < want {
                    break;
                }
            }
        }
    }

    let Some(samples) = samples else {
        bail!("{} has no data chunk", path.display());
    };
    Ok(samples)
}

fn parse_fmt(path: &Path, body: &[u8]) -> Result<WavEncoding> {
    if body.len() < 16 {
        bail!("{} has a truncated fmt chunk", path.display());
    }
    let format_code = u16::from_le_bytes([body[0], body[1]]);
    let channels = u16::from_le_bytes([body[2], body[3]]);
    let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let bits = u16::from_le_bytes([body[14], body[15]]);

    if rate != TARGET_RATE || channels != 1 {
        bail!("{}", wav_requirement(path, rate, channels));
    }

    let sub_code = sub_format_code(body);
    let encoding = match (format_code, bits) {
        (1, 16) => WavEncoding::Pcm16,
        (3, 32) => WavEncoding::F32,
        // WAVE_FORMAT_EXTENSIBLE keeps the real format code in the sub-format GUID.
        (0xFFFE, 16) if sub_code == Some(1) => WavEncoding::Pcm16,
        (0xFFFE, 32) if sub_code == Some(3) => WavEncoding::F32,
        _ => {
            tracing::debug!(format_code, bits, "unsupported WAV encoding");
            bail!("{}", wav_requirement(path, rate, channels));
        }
    };

    Ok(encoding)
}

/// The `KSDATAFORMAT_SUBTYPE_*` code of a WAVE_FORMAT_EXTENSIBLE `fmt` chunk, if present.
fn sub_format_code(body: &[u8]) -> Option<u32> {
    if body.len() < 40 {
        return None;
    }
    Some(u32::from_le_bytes([body[24], body[25], body[26], body[27]]))
}

/// Read a `data` chunk body as mono samples. A zero or `u32::MAX` length means "until EOF".
fn read_data(
    path: &Path,
    reader: &mut impl Read,
    len: u64,
    encoding: WavEncoding,
) -> Result<Vec<f32>> {
    let sample_bytes = encoding.sample_bytes();
    let unbounded = len == 0 || len == u64::from(u32::MAX);
    let capacity = if unbounded {
        1 << 16
    } else {
        (len as usize / sample_bytes).min(1 << 22)
    };
    let mut samples: Vec<f32> = Vec::with_capacity(capacity);
    let mut block = vec![0u8; WAV_BLOCK_BYTES];
    let mut remaining = if unbounded { u64::MAX } else { len };

    while remaining > 0 {
        let want = remaining.min(WAV_BLOCK_BYTES as u64) as usize;
        let filled = read_full(reader, &mut block[..want]);
        let filled = filled.map_err(|err| io_error(path, err))?;
        if filled == 0 {
            break;
        }
        encoding.decode(&block[..filled], &mut samples);
        remaining -= filled as u64;
        if filled < want {
            tracing::warn!(
                file = %path.display(),
                read = filled,
                expected = want,
                "the WAV data ends before its declared length; the rest is ignored"
            );
            break;
        }
    }

    Ok(samples)
}

/// Fill `buf` unless the reader ends first; returns how many bytes were read.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// Skip `count` bytes, returning how many could actually be skipped.
fn skip_bytes(reader: &mut impl Read, count: u64) -> std::io::Result<u64> {
    let mut scratch = [0u8; 8 * 1024];
    let mut left = count;
    while left > 0 {
        let want = left.min(scratch.len() as u64) as usize;
        match reader.read(&mut scratch[..want]) {
            Ok(0) => break,
            Ok(count) => left -= count as u64,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(count - left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn temp_path(name: &str) -> PathBuf {
        let id = std::process::id();
        std::env::temp_dir().join(format!("lingua-audio-{id}-{name}"))
    }

    /// Write a WAV file with a canonical 16-byte `fmt ` chunk and a single `data` chunk.
    fn write_wav(path: &Path, spec: WavSpec, data: &[u8]) {
        let size = data.len() as u32;
        let riff_len = 36 + size;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&riff_len.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&spec.format_code.to_le_bytes());
        out.extend_from_slice(&spec.channels.to_le_bytes());
        out.extend_from_slice(&spec.rate.to_le_bytes());
        out.extend_from_slice(&spec.byte_rate().to_le_bytes());
        out.extend_from_slice(&spec.block_align().to_le_bytes());
        out.extend_from_slice(&spec.bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(data);
        std::fs::write(path, &out).unwrap();
    }

    struct WavSpec {
        rate: u32,
        channels: u16,
        format_code: u16,
        bits: u16,
    }

    impl WavSpec {
        fn block_align(&self) -> u16 {
            self.channels * self.bits / 8
        }

        fn byte_rate(&self) -> u32 {
            self.rate * u32::from(self.block_align())
        }
    }

    fn pcm16(rate: u32, channels: u16) -> WavSpec {
        WavSpec {
            rate,
            channels,
            format_code: 1,
            bits: 16,
        }
    }

    #[test]
    fn capture_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Capture>();
    }

    #[test]
    fn recording_limits_are_the_contract_values() {
        // FR-018: 0.3 s is the shortest recording, 5 minutes the longest.
        assert_eq!(MIN_RECORDING_SAMPLES, u64::from(TARGET_RATE) * 3 / 10);
        assert_eq!(MAX_RECORDING_SECS, 300);
        assert_eq!(CHUNK_SAMPLES, TARGET_RATE as usize / 10);
    }

    #[test]
    fn chunks_are_100_ms_and_keep_the_remainder() {
        let count = CHUNK_SAMPLES * 2 + 500;
        let samples: Vec<f32> = (0..count).map(|i| i as f32).collect();
        let parts = chunks(&samples);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].samples.len(), CHUNK_SAMPLES);
        assert_eq!(parts[1].samples.len(), CHUNK_SAMPLES);
        assert_eq!(parts[2].samples.len(), 500);
        assert_eq!(parts[0].samples, samples[..CHUNK_SAMPLES]);
        assert_eq!(parts[1].samples, samples[CHUNK_SAMPLES..CHUNK_SAMPLES * 2]);
        let rest = &samples[CHUNK_SAMPLES * 2..];
        assert_eq!(parts[2].samples, rest);
    }

    #[test]
    fn chunks_of_an_exact_multiple_have_no_short_chunk() {
        let samples = vec![0.25f32; CHUNK_SAMPLES * 3];
        let parts = chunks(&samples);
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert_eq!(part.samples.len(), CHUNK_SAMPLES);
        }
    }

    #[test]
    fn chunks_of_empty_input_are_empty() {
        assert!(chunks(&[]).is_empty());
    }

    #[test]
    fn rms_matches_a_constant_amplitude_buffer() {
        let flat = vec![0.5f32; CHUNK_SAMPLES];
        let parts = chunks(&flat);
        assert_eq!(parts.len(), 1);
        assert!((parts[0].rms - 0.5).abs() < 1e-6);

        // Half a silent chunk: the mean square is halved, so the rms drops by sqrt(2).
        let mut mixed = vec![0.5f32; CHUNK_SAMPLES];
        mixed[CHUNK_SAMPLES / 2..].fill(0.0);
        let parts = chunks(&mixed);
        assert!((parts[0].rms - 0.125f32.sqrt()).abs() < 1e-6);
    }

    #[test]
    fn rms_is_clamped_to_one() {
        let parts = chunks(&[4.0f32, -4.0]);
        assert_eq!(parts[0].rms, 1.0);
    }

    #[test]
    fn reads_16k_mono_pcm16_wav() {
        let raw = [-32768i16, -16384, 0, 16384, 32767];
        let data: Vec<u8> = raw.into_iter().flat_map(i16::to_le_bytes).collect();
        let path = temp_path("pcm16.wav");
        write_wav(&path, pcm16(TARGET_RATE, 1), &data);

        let samples = read_wav_16k_mono(&path).unwrap();
        assert_eq!(samples.len(), 5);
        assert_eq!(samples[0], -1.0);
        assert_eq!(samples[1], -0.5);
        assert_eq!(samples[2], 0.0);
        assert_eq!(samples[3], 0.5);
        let top: f32 = 32_767.0 / 32_768.0;
        assert!((samples[4] - top).abs() < 1e-9);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn reads_16k_mono_float32_wav() {
        let raw = [0.25f32, -0.75, 1.0];
        let data: Vec<u8> = raw.into_iter().flat_map(f32::to_le_bytes).collect();
        let path = temp_path("f32.wav");
        let spec = WavSpec {
            rate: TARGET_RATE,
            channels: 1,
            format_code: 3,
            bits: 32,
        };
        write_wav(&path, spec, &data);

        let samples = read_wav_16k_mono(&path).unwrap();
        assert_eq!(samples, vec![0.25, -0.75, 1.0]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_48k_stereo_wav_with_the_contract_literal() {
        let path = temp_path("48k-stereo.wav");
        write_wav(&path, pcm16(48_000, 2), &[0u8; 64]);

        let err = read_wav_16k_mono(&path).unwrap_err();
        let err = err.to_string();
        let name = path.display().to_string();
        assert!(err.starts_with(&name), "{err}");
        let tail = "must be 16 kHz mono PCM WAV (got 48000 Hz, 2 channels)";
        assert!(err.ends_with(tail), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_a_non_wave_file() {
        let path = temp_path("not-a-wave.wav");
        std::fs::write(&path, b"not a wave file at all").unwrap();

        let err = read_wav_16k_mono(&path).unwrap_err();
        assert!(err.to_string().contains("RIFF/WAVE"), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn pipeline_downmixes_stereo_and_emits_100_ms_chunks() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut pipeline = Pipeline::new(SampleFormat::F32, 2, TARGET_RATE, tx).unwrap();
        let mut bytes = Vec::new();
        for _ in 0..CHUNK_SAMPLES {
            bytes.extend_from_slice(&0.5f32.to_ne_bytes());
            bytes.extend_from_slice(&0.25f32.to_ne_bytes());
        }
        pipeline.push_bytes(&bytes).unwrap();

        let chunk = rx.try_recv().unwrap();
        assert_eq!(chunk.samples.len(), CHUNK_SAMPLES);
        assert!((chunk.samples[0] - 0.375).abs() < 1e-6);
        assert!((chunk.rms - 0.375).abs() < 1e-6);
        assert!(rx.try_recv().is_err());

        // A partial chunk is held back until the recording stops.
        pipeline.push_bytes(&bytes[..8]).unwrap();
        assert!(rx.try_recv().is_err());
        pipeline.flush().unwrap();
        let tail = rx.try_recv().unwrap();
        assert_eq!(tail.samples.len(), 1);
        assert!((tail.samples[0] - 0.375).abs() < 1e-6);
    }

    #[test]
    fn pipeline_decodes_i16_pcm() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut pipeline = Pipeline::new(SampleFormat::I16, 1, TARGET_RATE, tx).unwrap();
        let samples: Vec<i16> = vec![16_384; CHUNK_SAMPLES];
        let bytes: Vec<u8> = samples.into_iter().flat_map(i16::to_ne_bytes).collect();
        pipeline.push_bytes(&bytes).unwrap();

        let chunk = rx.try_recv().unwrap();
        assert_eq!(chunk.samples.len(), CHUNK_SAMPLES);
        assert!((chunk.samples[0] - 0.5).abs() < 1e-6);
        assert!((chunk.rms - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pipeline_resamples_48k_to_16k_without_losing_duration() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut pipeline = Pipeline::new(SampleFormat::F32, 1, 48_000, tx).unwrap();
        let mut bytes = Vec::new();
        for _ in 0..48_000 {
            bytes.extend_from_slice(&0.5f32.to_ne_bytes());
        }
        pipeline.push_bytes(&bytes).unwrap();
        pipeline.flush().unwrap();

        let mut all = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            all.extend_from_slice(&chunk.samples);
        }
        // One second in, one second out: the resampler's delay is dropped and its tail pumped.
        let len = all.len();
        assert!((15_990..=16_010).contains(&len), "resampled {len} samples");
        assert!(all.iter().all(|sample| sample.is_finite()));
        let middle = &all[len / 4..len * 3 / 4];
        let flat = middle.iter().all(|sample| (*sample - 0.5).abs() < 0.02);
        assert!(flat, "a constant signal was resampled to something else");
    }

    #[test]
    fn pipeline_rejects_an_unsupported_sample_format() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let rejected = Pipeline::new(SampleFormat::F64, 1, TARGET_RATE, tx);
        let err = match rejected {
            Ok(_) => panic!("F64 must not be accepted"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("unsupported sample format"),
            "{err}"
        );
    }
}
