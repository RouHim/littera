//! Bundled snapshot of the `transcribe.cpp` model catalog (FR-001, Scenario 1).
//!
//! The snapshot is compiled-in data: the list, its ratings and its metadata need
//! no network and no upstream query (FR-012), and every artifact pins the exact
//! byte size and SHA-256 of the published file so the verified download pipeline
//! in [`crate::models`] can reject anything that is not byte-identical to it.
//!
//! Two properties of the upstream data drive the code here and are therefore
//! encoded as data rather than as assumptions:
//!
//! * **Language spellings differ per variant.** The engine's C API rejects a
//!   language hint that is not byte-equal to one of the entries the GGUF
//!   advertises in `general.languages` (used by both `transcribe_run` and
//!   `transcribe_stream_begin`): Nemotron 3.5 advertises locale codes
//!   (`en-US`, `de-DE`, …), Voxtral bare ones (`en`, `de`, …) and the
//!   English-only variants exactly `["en"]`. [`language_hint`] therefore only
//!   ever returns a spelling the variant advertises, and [`language_warning`]
//!   reports the mismatch instead of letting the engine refuse the hint. The
//!   configured side is [`Language`] (see [`crate::config`]): one locale per
//!   language these lists can serve, so the language selectors and the list
//!   filter offer their union without a per-variant special case.
//! * **Measurements are sparse.** Only some quants of some variants have a
//!   published number; a missing one displays as `—` and is never substituted
//!   (FR-004, SC-007).
//! * **Which quant is worth its size is a per-model question.** The error a
//!   quantisation costs is measured per variant and the speed rows do not cover
//!   every quant, so [`Variant::recommended`] names one quant per variant instead
//!   of a single global constant, and [`recommended`] resolves it. The compute
//!   backend is not part of that answer — the measurements do not give it one.

use crate::config::Language;

/// Provenance of the bundled snapshot: the closing line of the model list and
/// the snapshot row of the Storage card.
pub const SNAPSHOT_VERSION: &str = "transcribe.cpp v0.4.0 catalog, pinned 2026-10-16";

/// Catalog id of the default model artifact (FR-008; unchanged default).
pub const DEFAULT_ARTIFACT_ID: &str = "nemotron-3.5-asr-streaming-0.6b-Q8_0";

/// The reference rig of the speed rating (FR-004).
pub const SPEED_MACHINE_LABEL: &str = "AMD Ryzen 7 PRO 4750U";

/// One downloadable artifact: a concrete GGUF file of a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Artifact {
    /// Catalog id, `"<variant>-<quant>"`; this is what the config persists (FR-008).
    pub id: &'static str,
    /// Quantisation, e.g. `"Q8_0"`.
    pub quant: &'static str,
    /// Flat file name inside the model directory.
    pub file_name: &'static str,
    /// Commit-pinned download URL.
    pub url: &'static str,
    /// Exact artifact size in bytes.
    pub size_bytes: u64,
    /// Exact SHA-256 of the artifact, lower-case hex.
    pub sha256: &'static str,
}

/// One published accuracy measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Accuracy {
    /// Benchmark corpus, e.g. `"fleurs"`.
    pub dataset: &'static str,
    /// Split of that corpus, e.g. `"test-clean"`.
    pub split: &'static str,
    /// Language of the measurement, as the corpus names it (`"en"`, `"de"`).
    pub language: &'static str,
    /// Quant the measurement was taken with.
    pub quant: &'static str,
    /// Error metric: `"wer"` or `"cer"`.
    pub metric: &'static str,
    /// The published error rate in percent (lower is better).
    pub err_pct: f32,
}

/// One published speed measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speed {
    /// Backend the measurement used: `"vulkan"` or `"cpu"`.
    pub backend: &'static str,
    /// Quant the measurement was taken with.
    pub quant: &'static str,
    /// Throughput as a multiple of realtime (higher is better).
    pub xrt: f32,
}

/// One selectable model variant with its metadata and its artifacts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Variant {
    /// Variant slug as the upstream catalog uses it.
    pub id: &'static str,
    /// Display name on the card.
    pub label: &'static str,
    /// One-line description on the card (FR-003).
    pub note: &'static str,
    /// Upstream family, part of the search text (FR-005).
    pub family: &'static str,
    /// Licence of the published weights.
    pub license: &'static str,
    /// Parameter count of the variant.
    pub params: u64,
    /// Languages exactly as the GGUF's `general.languages` lists them.
    pub languages: &'static [&'static str],
    /// Offered quants, catalog order.
    pub downloads: &'static [Artifact],
    /// The quant this variant's card preselects (FR-003).
    ///
    /// These are data, not taste, and one rule produced them: take the smallest
    /// offered quant that is *measured on both axes* — its error within 0.05 pp of
    /// the Q8_0 reference on every measured cell of that variant (single-cell
    /// measurements cannot resolve less), and a published speed row — and never
    /// prefer an unmeasured quant over a measured one. Where nothing smaller
    /// qualifies, the reference itself (Q8_0) is the recommendation. The test
    /// module re-derives the table from this rule, so new upstream numbers cannot
    /// drift away from it silently.
    ///
    /// **The compute backend does not enter this decision.** A quantisation costs
    /// the same error on every backend (the weights are identical, and the
    /// accuracy rows carry no backend at all), and the measured throughput
    /// difference between Q8_0 and Q4_K_M does not favour the CPU: on the 0.6B
    /// family Q4_K_M is 1.0–3.9 % *slower* on either backend, and the one variant
    /// where it wins big — Voxtral Mini 4B, memory-bound, +15 % Vulkan and +26 %
    /// CPU — lands on Q4_K_M anyway. A backend-dependent answer would be a second
    /// answer to a question the measurements do not ask; it comes back when a
    /// per-quant × per-backend measurement makes one, which is also when the
    /// rule's test fails.
    pub recommended: &'static str,
    /// Published accuracy measurements of the offered quants.
    pub accuracy: &'static [Accuracy],
    /// Published speed measurements on [`SPEED_MACHINE_LABEL`].
    pub speed: &'static [Speed],
    /// Published headline cell of the catalog record: (dataset, split, language).
    pub headline: (&'static str, &'static str, &'static str),
}

/// A resolved selection: the variant plus the artifact (quant) it will run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub variant: &'static Variant,
    pub artifact: &'static Artifact,
}

impl Selection {
    /// `"<variant> — <quant>"`, the label the download notices use.
    pub fn label(self) -> String {
        format!("{} — {}", self.variant.label, self.artifact.quant)
    }
}

/// Every offered variant, ascending by label — which is also the default sort.
///
/// Only streaming-capable, transcribing ASR variants that the bundled engine can
/// run with the app's language handling are listed (FR-002); the quant set is
/// restricted to Q8_0/Q6_K/Q5_K_M/Q4_K_M (and Moonshine Streaming, which the
/// publisher only offers as Q8_0 among those).
pub const VARIANTS: [Variant; 8] = [
    Variant {
        id: "moonshine-streaming-medium",
        // The publisher offers no other quant, so there is nothing to choose.
        recommended: "Q8_0",
        label: "Moonshine Streaming Medium",
        note: "English only. Best accuracy of the three Moonshine Streaming models.",
        family: "moonshine_streaming",
        license: "MIT",
        params: 265_898_113,
        languages: &["en"],
        downloads: &[Artifact {
            id: "moonshine-streaming-medium-Q8_0",
            quant: "Q8_0",
            file_name: "moonshine-streaming-medium-Q8_0.gguf",
            url: "https://huggingface.co/handy-computer/moonshine-streaming-medium-gguf/resolve/56302cde2ae162d1357a833d91775bcd06e91e1b/moonshine-streaming-medium-Q8_0.gguf",
            size_bytes: 295_793_568,
            sha256: "f7c9564249b508f6012927ec4f9e536087da53a7047f858ca9975bea5f75299e",
        }],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 7.87,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 2.16,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 8.90,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 5.86,
            },
        ],
        headline: ("librispeech", "test-clean", "en"),
    },
    Variant {
        id: "moonshine-streaming-small",
        // As with Medium: Q8_0 is the only published artifact.
        recommended: "Q8_0",
        label: "Moonshine Streaming Small",
        note: "English only. Roughly halves the tiny model's word error rate.",
        family: "moonshine_streaming",
        license: "MIT",
        params: 140_135_225,
        languages: &["en"],
        downloads: &[Artifact {
            id: "moonshine-streaming-small-Q8_0",
            quant: "Q8_0",
            file_name: "moonshine-streaming-small-Q8_0.gguf",
            url: "https://huggingface.co/handy-computer/moonshine-streaming-small-gguf/resolve/e53f28b0cb90308390c852068b950a1aadd238b0/moonshine-streaming-small-Q8_0.gguf",
            size_bytes: 198_506_848,
            sha256: "d03670f69629b649085d0f44a63d97668b4119117cc9611a4e4ad94341713dfc",
        }],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 8.55,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 2.54,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 14.16,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 9.32,
            },
        ],
        headline: ("librispeech", "test-clean", "en"),
    },
    Variant {
        id: "moonshine-streaming-tiny",
        // As with Medium: Q8_0 is the only published artifact.
        recommended: "Q8_0",
        label: "Moonshine Streaming Tiny",
        note: "English only. 44 M parameters — the smallest and fastest model here.",
        family: "moonshine_streaming",
        license: "MIT",
        params: 44_054_721,
        languages: &["en"],
        downloads: &[Artifact {
            id: "moonshine-streaming-tiny-Q8_0",
            quant: "Q8_0",
            file_name: "moonshine-streaming-tiny-Q8_0.gguf",
            url: "https://huggingface.co/handy-computer/moonshine-streaming-tiny-gguf/resolve/4125c28c22a76494162094ab3566387cd92fb2f5/moonshine-streaming-tiny-Q8_0.gguf",
            size_bytes: 50_462_816,
            sha256: "930e4622ad3a24158b91406c30c977fa6a26b34cb32d6ac3e57cfb23383a869e",
        }],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 18.18,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 4.52,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 38.30,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 37.25,
            },
        ],
        headline: ("librispeech", "test-clean", "en"),
    },
    Variant {
        id: "multitalker-parakeet-streaming-0.6b-v1",
        // Q4_K_M measures +0.00 pp (2.18 % wer, librispeech/en) at -256 MB.
        recommended: "Q4_K_M",
        label: "Multitalker Parakeet Streaming 0.6B v1",
        note: "English only. Streaming ASR from NVIDIA's multitalker checkpoint, run single-speaker.",
        family: "parakeet",
        license: "NVIDIA Open Model License",
        params: 739_989_513,
        languages: &["en"],
        downloads: &[
            Artifact {
                id: "multitalker-parakeet-streaming-0.6b-v1-Q8_0",
                quant: "Q8_0",
                file_name: "multitalker-parakeet-streaming-0.6b-v1-Q8_0.gguf",
                url: "https://huggingface.co/handy-computer/multitalker-parakeet-streaming-0.6b-v1-gguf/resolve/e9671411e593f17994f37540ac061e6b5dfe043b/bundle/multitalker-parakeet-streaming-0.6b-v1-Q8_0.gguf",
                size_bytes: 873_443_456,
                sha256: "cf83cedc6777d2528a4d2a8a4bf476174ef7e5c65b86bc6c955f4cc9f5951ac5",
            },
            Artifact {
                id: "multitalker-parakeet-streaming-0.6b-v1-Q6_K",
                quant: "Q6_K",
                file_name: "multitalker-parakeet-streaming-0.6b-v1-Q6_K.gguf",
                url: "https://huggingface.co/handy-computer/multitalker-parakeet-streaming-0.6b-v1-gguf/resolve/e9671411e593f17994f37540ac061e6b5dfe043b/bundle/multitalker-parakeet-streaming-0.6b-v1-Q6_K.gguf",
                size_bytes: 743_197_824,
                sha256: "d8db95ba257e370d910f42e8197dfc65fb5b879e09e4b13aefaac06af3555fa1",
            },
            Artifact {
                id: "multitalker-parakeet-streaming-0.6b-v1-Q5_K_M",
                quant: "Q5_K_M",
                file_name: "multitalker-parakeet-streaming-0.6b-v1-Q5_K_M.gguf",
                url: "https://huggingface.co/handy-computer/multitalker-parakeet-streaming-0.6b-v1-gguf/resolve/e9671411e593f17994f37540ac061e6b5dfe043b/bundle/multitalker-parakeet-streaming-0.6b-v1-Q5_K_M.gguf",
                size_bytes: 681_209_984,
                sha256: "38c98bc0715ee95b4271952630661050a8c3c37f64b8b178c62b8b699668cbdb",
            },
            Artifact {
                id: "multitalker-parakeet-streaming-0.6b-v1-Q4_K_M",
                quant: "Q4_K_M",
                file_name: "multitalker-parakeet-streaming-0.6b-v1-Q4_K_M.gguf",
                url: "https://huggingface.co/handy-computer/multitalker-parakeet-streaming-0.6b-v1-gguf/resolve/e9671411e593f17994f37540ac061e6b5dfe043b/bundle/multitalker-parakeet-streaming-0.6b-v1-Q4_K_M.gguf",
                size_bytes: 617_132_160,
                sha256: "d24307ac22e9e691c146a7e2339891638a44a8492a765e4b6c11cff8960cd998",
            },
        ],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 6.52,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 2.18,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q6_K",
                metric: "wer",
                err_pct: 2.20,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q5_K_M",
                metric: "wer",
                err_pct: 2.18,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q4_K_M",
                metric: "wer",
                err_pct: 2.18,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 27.61,
            },
            Speed {
                backend: "vulkan",
                quant: "Q4_K_M",
                xrt: 27.12,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 12.13,
            },
            Speed {
                backend: "cpu",
                quant: "Q4_K_M",
                xrt: 11.95,
            },
        ],
        headline: ("librispeech", "test-clean", "en"),
    },
    Variant {
        id: "nemotron-3.5-asr-streaming-0.6b",
        // Q5_K_M measures +0.05 pp (inside the band) but has no published speed
        // row; Q4_K_M costs +0.25 pp. The rule keeps the reference until a
        // smaller quant is measured on both axes.
        recommended: "Q8_0",
        label: "Nemotron 3.5 ASR Streaming 0.6B",
        note: "Multilingual (32 locales). Best quality.",
        family: "parakeet",
        license: "OpenMDW-1.1",
        params: 637_991_968,
        languages: &[
            "en-US", "en-GB", "es-US", "es-ES", "fr-FR", "fr-CA", "it-IT", "pt-BR", "pt-PT",
            "nl-NL", "de-DE", "tr-TR", "ru-RU", "ar-AR", "hi-IN", "ja-JP", "ko-KR", "vi-VN",
            "uk-UA", "pl-PL", "sv-SE", "cs-CZ", "nb-NO", "da-DK", "bg-BG", "fi-FI", "hr-HR",
            "sk-SK", "zh-CN", "hu-HU", "ro-RO", "et-EE",
        ],
        downloads: &[
            Artifact {
                id: "nemotron-3.5-asr-streaming-0.6b-Q8_0",
                quant: "Q8_0",
                file_name: "nemotron-3.5-asr-streaming-0.6b-Q8_0.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf/resolve/53ce288f69326b415aeef030cfcb02a08a8096dd/nemotron-3.5-asr-streaming-0.6b-Q8_0.gguf",
                size_bytes: 751_094_240,
                sha256: "b94545b313b3223fda7b2857a52681da813935c2127643d1e9ff0c23d988089c",
            },
            Artifact {
                id: "nemotron-3.5-asr-streaming-0.6b-Q6_K",
                quant: "Q6_K",
                file_name: "nemotron-3.5-asr-streaming-0.6b-Q6_K.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf/resolve/53ce288f69326b415aeef030cfcb02a08a8096dd/nemotron-3.5-asr-streaming-0.6b-Q6_K.gguf",
                size_bytes: 621_356_512,
                sha256: "4ff802c6207c4a7df23242003fd2aa849a1ab02bba6bc80c3db02e7e82606c28",
            },
            Artifact {
                id: "nemotron-3.5-asr-streaming-0.6b-Q5_K_M",
                quant: "Q5_K_M",
                file_name: "nemotron-3.5-asr-streaming-0.6b-Q5_K_M.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf/resolve/53ce288f69326b415aeef030cfcb02a08a8096dd/nemotron-3.5-asr-streaming-0.6b-Q5_K_M.gguf",
                size_bytes: 559_647_200,
                sha256: "86429e8c4f7fdcf9b3312269ad1ca6669478ba7805331c4aea7a2e33e9910d65",
            },
            Artifact {
                id: "nemotron-3.5-asr-streaming-0.6b-Q4_K_M",
                quant: "Q4_K_M",
                file_name: "nemotron-3.5-asr-streaming-0.6b-Q4_K_M.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf/resolve/53ce288f69326b415aeef030cfcb02a08a8096dd/nemotron-3.5-asr-streaming-0.6b-Q4_K_M.gguf",
                size_bytes: 495_831_520,
                sha256: "41c99fa5fb6f3d35f68e79adc3e755eca2232a8d921178bd647b71194792b8fd",
            },
        ],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "de",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 10.33,
            },
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 7.90,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 3.05,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q6_K",
                metric: "wer",
                err_pct: 3.08,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q5_K_M",
                metric: "wer",
                err_pct: 3.10,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q4_K_M",
                metric: "wer",
                err_pct: 3.30,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 17.09,
            },
            Speed {
                backend: "vulkan",
                quant: "Q4_K_M",
                xrt: 16.88,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 9.62,
            },
            Speed {
                backend: "cpu",
                quant: "Q4_K_M",
                xrt: 9.45,
            },
        ],
        headline: ("fleurs", "test", "en"),
    },
    Variant {
        id: "nemotron-speech-streaming-en-0.6b",
        // Q5_K_M measures +0.03 pp (inside the band) but has no published speed
        // row; Q4_K_M costs +0.07 pp, outside the band.
        recommended: "Q8_0",
        label: "Nemotron Speech Streaming EN 0.6B",
        note: "English only. Punctuated, low-latency English dictation.",
        family: "parakeet",
        license: "NVIDIA Open Model License",
        params: 618_079_745,
        languages: &["en"],
        downloads: &[
            Artifact {
                id: "nemotron-speech-streaming-en-0.6b-Q8_0",
                quant: "Q8_0",
                file_name: "nemotron-speech-streaming-en-0.6b-Q8_0.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-speech-streaming-en-0.6b-gguf/resolve/7df420cc3fb915a3b334560dac712af44fc41ef5/nemotron-speech-streaming-en-0.6b-Q8_0.gguf",
                size_bytes: 729_650_176,
                sha256: "90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110",
            },
            Artifact {
                id: "nemotron-speech-streaming-en-0.6b-Q6_K",
                quant: "Q6_K",
                file_name: "nemotron-speech-streaming-en-0.6b-Q6_K.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-speech-streaming-en-0.6b-gguf/resolve/7df420cc3fb915a3b334560dac712af44fc41ef5/nemotron-speech-streaming-en-0.6b-Q6_K.gguf",
                size_bytes: 600_420_352,
                sha256: "b31fcc5e9f3b00cb33c10ff0f05d7e71265f160eeed6b497a65cd296dddd21f3",
            },
            Artifact {
                id: "nemotron-speech-streaming-en-0.6b-Q5_K_M",
                quant: "Q5_K_M",
                file_name: "nemotron-speech-streaming-en-0.6b-Q5_K_M.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-speech-streaming-en-0.6b-gguf/resolve/7df420cc3fb915a3b334560dac712af44fc41ef5/nemotron-speech-streaming-en-0.6b-Q5_K_M.gguf",
                size_bytes: 538_989_568,
                sha256: "d6b8b36f9aca6a751779d064a29f6583f3e82c0c15646904383c8436a25c1489",
            },
            Artifact {
                id: "nemotron-speech-streaming-en-0.6b-Q4_K_M",
                quant: "Q4_K_M",
                file_name: "nemotron-speech-streaming-en-0.6b-Q4_K_M.gguf",
                url: "https://huggingface.co/handy-computer/nemotron-speech-streaming-en-0.6b-gguf/resolve/7df420cc3fb915a3b334560dac712af44fc41ef5/nemotron-speech-streaming-en-0.6b-Q4_K_M.gguf",
                size_bytes: 475_436_032,
                sha256: "dc959ca31499b114e395c44eb4f0778968f20e5cfb03305a08a39925b2da8e1e",
            },
        ],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 6.43,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 2.31,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q6_K",
                metric: "wer",
                err_pct: 2.29,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q5_K_M",
                metric: "wer",
                err_pct: 2.34,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q4_K_M",
                metric: "wer",
                err_pct: 2.38,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 28.32,
            },
            Speed {
                backend: "vulkan",
                quant: "Q4_K_M",
                xrt: 28.01,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 12.33,
            },
            Speed {
                backend: "cpu",
                quant: "Q4_K_M",
                xrt: 12.06,
            },
        ],
        headline: ("librispeech", "test-clean", "en"),
    },
    Variant {
        id: "parakeet-unified-en-0.6b",
        // Q4_K_M measures +0.02 pp (1.60 -> 1.62 % wer) at -254 MB.
        recommended: "Q4_K_M",
        label: "Parakeet Unified EN 0.6B",
        note: "English only. Highest English accuracy in the set.",
        family: "parakeet",
        license: "NVIDIA Open Model License",
        params: 618_309_121,
        languages: &["en"],
        downloads: &[
            Artifact {
                id: "parakeet-unified-en-0.6b-Q8_0",
                quant: "Q8_0",
                file_name: "parakeet-unified-en-0.6b-Q8_0.gguf",
                url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/843b1b175ace04f5a89d379c79e2518c0d065b25/parakeet-unified-en-0.6b-Q8_0.gguf",
                size_bytes: 731_357_568,
                sha256: "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795",
            },
            Artifact {
                id: "parakeet-unified-en-0.6b-Q6_K",
                quant: "Q6_K",
                file_name: "parakeet-unified-en-0.6b-Q6_K.gguf",
                url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/843b1b175ace04f5a89d379c79e2518c0d065b25/parakeet-unified-en-0.6b-Q6_K.gguf",
                size_bytes: 602_191_232,
                sha256: "912c42ec3fd1afa31b7eda45684577feadf413249ca767fa912a89c4810e38d0",
            },
            Artifact {
                id: "parakeet-unified-en-0.6b-Q5_K_M",
                quant: "Q5_K_M",
                file_name: "parakeet-unified-en-0.6b-Q5_K_M.gguf",
                url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/843b1b175ace04f5a89d379c79e2518c0d065b25/parakeet-unified-en-0.6b-Q5_K_M.gguf",
                size_bytes: 540_795_264,
                sha256: "f9def6f9b4e83ab7d006df3e1b676dfa1f973a3b6da232a9c99fcaa66bcd2836",
            },
            Artifact {
                id: "parakeet-unified-en-0.6b-Q4_K_M",
                quant: "Q4_K_M",
                file_name: "parakeet-unified-en-0.6b-Q4_K_M.gguf",
                url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/843b1b175ace04f5a89d379c79e2518c0d065b25/parakeet-unified-en-0.6b-Q4_K_M.gguf",
                size_bytes: 477_274_496,
                sha256: "a8bf3de2b393bd14ead5a858c3748d5e3b07a20fdeabdd3b498fba4f463fa929",
            },
        ],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 3.99,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 1.60,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q6_K",
                metric: "wer",
                err_pct: 1.61,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q5_K_M",
                metric: "wer",
                err_pct: 1.58,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q4_K_M",
                metric: "wer",
                err_pct: 1.62,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 26.01,
            },
            Speed {
                backend: "vulkan",
                quant: "Q4_K_M",
                xrt: 25.74,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 12.46,
            },
            Speed {
                backend: "cpu",
                quant: "Q4_K_M",
                xrt: 11.97,
            },
        ],
        headline: ("librispeech", "test-clean", "en"),
    },
    Variant {
        id: "voxtral-mini-4b-realtime-2602",
        // +0.01 pp, -1.9 GB, and the only measured speed win (0.78x -> 0.90x Vulkan, 0.61x -> 0.77x CPU).
        recommended: "Q4_K_M",
        label: "Voxtral Mini 4B Realtime (2602)",
        note: "Multilingual realtime audio LLM (13 languages). Large; wants a fast GPU.",
        family: "voxtral_realtime",
        license: "Apache-2.0",
        params: 4_429_707_024,
        languages: &[
            "en", "fr", "es", "de", "ru", "zh", "ja", "it", "pt", "nl", "ar", "hi", "ko",
        ],
        downloads: &[
            Artifact {
                id: "voxtral-mini-4b-realtime-2602-Q8_0",
                quant: "Q8_0",
                file_name: "Voxtral-Mini-4B-Realtime-2602-Q8_0.gguf",
                url: "https://huggingface.co/handy-computer/Voxtral-Mini-4B-Realtime-2602-gguf/resolve/7fd3d424fc558edca2c53808578b0541ef52a81a/Voxtral-Mini-4B-Realtime-2602-Q8_0.gguf",
                size_bytes: 4_731_791_648,
                sha256: "6fb22249463c1e7a600d920ad732e2dd0a231510e9db59fb8061d3d1d48d0c67",
            },
            Artifact {
                id: "voxtral-mini-4b-realtime-2602-Q6_K",
                quant: "Q6_K",
                file_name: "Voxtral-Mini-4B-Realtime-2602-Q6_K.gguf",
                url: "https://huggingface.co/handy-computer/Voxtral-Mini-4B-Realtime-2602-gguf/resolve/7fd3d424fc558edca2c53808578b0541ef52a81a/Voxtral-Mini-4B-Realtime-2602-Q6_K.gguf",
                size_bytes: 3_661_018_912,
                sha256: "92038784cffa20ea8ee925f10fd28c110285124d94730eacce48bab6c214fa48",
            },
            Artifact {
                id: "voxtral-mini-4b-realtime-2602-Q5_K_M",
                quant: "Q5_K_M",
                file_name: "Voxtral-Mini-4B-Realtime-2602-Q5_K_M.gguf",
                url: "https://huggingface.co/handy-computer/Voxtral-Mini-4B-Realtime-2602-gguf/resolve/7fd3d424fc558edca2c53808578b0541ef52a81a/Voxtral-Mini-4B-Realtime-2602-Q5_K_M.gguf",
                size_bytes: 3_281_439_008,
                sha256: "e20a7582c5cf8159454c909b7f57b184b287cb3d7c3ec85744727a2cec51b08f",
            },
            Artifact {
                id: "voxtral-mini-4b-realtime-2602-Q4_K_M",
                quant: "Q4_K_M",
                file_name: "Voxtral-Mini-4B-Realtime-2602-Q4_K_M.gguf",
                url: "https://huggingface.co/handy-computer/Voxtral-Mini-4B-Realtime-2602-gguf/resolve/7fd3d424fc558edca2c53808578b0541ef52a81a/Voxtral-Mini-4B-Realtime-2602-Q4_K_M.gguf",
                size_bytes: 2_830_493_984,
                sha256: "39dc1f65539373a406edea7490505822d77c12edff521744678717eef4da4723",
            },
        ],
        accuracy: &[
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "de",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 4.24,
            },
            Accuracy {
                dataset: "fleurs",
                split: "test",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 6.27,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q8_0",
                metric: "wer",
                err_pct: 2.07,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q6_K",
                metric: "wer",
                err_pct: 2.08,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q5_K_M",
                metric: "wer",
                err_pct: 2.08,
            },
            Accuracy {
                dataset: "librispeech",
                split: "test-clean",
                language: "en",
                quant: "Q4_K_M",
                metric: "wer",
                err_pct: 2.08,
            },
        ],
        speed: &[
            Speed {
                backend: "vulkan",
                quant: "Q8_0",
                xrt: 0.78,
            },
            Speed {
                backend: "vulkan",
                quant: "Q4_K_M",
                xrt: 0.90,
            },
            Speed {
                backend: "cpu",
                quant: "Q8_0",
                xrt: 0.61,
            },
            Speed {
                backend: "cpu",
                quant: "Q4_K_M",
                xrt: 0.77,
            },
        ],
        headline: ("librispeech", "test-clean", "en"),
    },
];

/// Onboarding keeps exactly its three curated models (FR-010).
pub const ONBOARDING_ARTIFACT_IDS: [&str; 3] = [
    "nemotron-3.5-asr-streaming-0.6b-Q8_0",
    "nemotron-3.5-asr-streaming-0.6b-Q4_K_M",
    "nemotron-speech-streaming-en-0.6b-Q8_0",
];

/// Resolve a catalog id (`"<variant>-<quant>"`) to its variant and artifact.
///
/// The variant slug is matched literally instead of splitting the id at a `-`:
/// slugs contain digits and dots (`nemotron-3.5-asr-streaming-0.6b`,
/// `voxtral-mini-4b-realtime-2602`), so only the quant that follows a matched
/// slug is a separator.
pub fn artifact(id: &str) -> Option<Selection> {
    for variant in VARIANTS.iter() {
        let Some(quant) = id
            .strip_prefix(variant.id)
            .and_then(|rest| rest.strip_prefix('-'))
        else {
            continue;
        };
        if let Some(artifact) = variant.downloads.iter().find(|entry| entry.quant == quant) {
            return Some(Selection { variant, artifact });
        }
    }
    None
}

/// The configured default artifact, falling back to the first offered one if the
/// snapshot is ever edited inconsistently, so this never panics.
pub fn default_selection() -> Selection {
    artifact(DEFAULT_ARTIFACT_ID).unwrap_or_else(|| Selection {
        variant: &VARIANTS[0],
        artifact: &VARIANTS[0].downloads[0],
    })
}

/// The variant/artifact that owns a model file name.
///
/// The engine re-resolves every load through this function, so a file that is
/// not part of the snapshot can never be loaded ([`crate::engine`]).
pub fn selection_for_file_name(file_name: &str) -> Option<Selection> {
    artifacts().find(|selection| selection.artifact.file_name == file_name)
}

/// Every offered artifact, in snapshot order (variants by label, quants by the
/// order each variant lists them).
pub fn artifacts() -> impl Iterator<Item = Selection> {
    VARIANTS.iter().flat_map(|variant| {
        variant
            .downloads
            .iter()
            .map(move |artifact| Selection { variant, artifact })
    })
}

/// The quant a card preselects: the variant's recommendation when it offers that
/// quant, otherwise its first artifact (FR-003).
pub fn recommended(variant: &'static Variant) -> &'static Artifact {
    variant
        .downloads
        .iter()
        .find(|artifact| artifact.quant == variant.recommended)
        .unwrap_or(&variant.downloads[0])
}

/// Whether the variant advertises the configured dictation language.
///
/// `Auto` always fits (the model picks the language per recording). An explicit
/// language fits when the variant lists it either as a locale of that language
/// or as the bare language itself: `de-DE` is served by `de-DE` and by `de`.
pub fn supports(variant: &Variant, language: Language) -> bool {
    let Some((_, bare)) = language.codes() else {
        return true;
    };
    variant.languages.iter().any(|advertised| {
        *advertised == bare
            || advertised
                .strip_prefix(bare)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// The language hint to send to the engine, in a spelling the variant accepts.
///
/// The C API refuses a hint that is not byte-equal to an advertised entry, so a
/// locale the variant does not list falls back to the bare language, and a
/// language it does not advertise at all yields `None` (run without a hint).
pub fn language_hint(variant: &Variant, language: Language) -> Option<&'static str> {
    let (locale, bare) = language.codes()?;
    let advertised = |wanted: &str| {
        variant
            .languages
            .iter()
            .copied()
            .find(|entry| *entry == wanted)
    };
    advertised(locale).or_else(|| advertised(bare))
}

/// The warning shown when the configured language is one the variant does not
/// serve. It never blocks a selection (FR-011): the caller renders it and the
/// engine runs without a language hint.
pub fn language_warning(variant: &Variant, language: Language) -> Option<String> {
    (!supports(variant, language)).then(|| {
        format!(
            "The selected model cannot transcribe {} — choose a different model.",
            language.display_name()
        )
    })
}

/// Parameter count in decimal units: `637_991_968` → `"638 M"`, `4_429_707_024`
/// → `"4.4 B"`.
pub fn params_label(params: u64) -> String {
    if params >= 1_000_000_000 {
        format!("{:.1} B", params as f64 / 1e9)
    } else {
        format!("{:.0} M", params as f64 / 1e6)
    }
}

/// The supported-language summary on the card (FR-003): how many languages the
/// variant advertises. Which ones those are is the language filter's job — the
/// card does not single one out.
pub fn languages_label(variant: &Variant) -> String {
    match variant.languages.len() {
        1 => "1 language".to_owned(),
        count => format!("{count} languages"),
    }
}

/// The accuracy cell to show for `selection` under `language` (FR-004).
///
/// The configured language wins over the headline result, and the selected quant
/// over any other quant; the headline dataset/split is preferred inside a set,
/// otherwise the alphabetically first one. `cpwer` rows (Multitalker's AMI
/// measurements) are excluded: they are not comparable with a WER and the card
/// shows one number.
pub fn quality(selection: Selection, language: Language) -> Option<&'static Accuracy> {
    let variant = selection.variant;
    let cells: Vec<&'static Accuracy> = variant
        .accuracy
        .iter()
        .filter(|cell| matches!(cell.metric, "wer" | "cer"))
        .collect();

    let primary = match language.codes() {
        Some((_, bare)) => bare,
        None => variant.headline.2,
    };
    let preferred = [primary, variant.headline.2];

    for wanted in preferred {
        for quant in [Some(selection.artifact.quant), None] {
            let set: Vec<&'static Accuracy> = cells
                .iter()
                .copied()
                .filter(|cell| cell.language == wanted)
                .filter(|cell| quant.is_none_or(|quant| cell.quant == quant))
                .collect();
            if set.is_empty() {
                continue;
            }
            let headline = set.iter().copied().find(|cell| {
                (cell.dataset, cell.split) == (variant.headline.0, variant.headline.1)
            });
            let picked = headline.or_else(|| {
                set.iter()
                    .copied()
                    .min_by_key(|cell| (cell.dataset, cell.split))
            });
            if let Some(picked) = picked {
                return Some(picked);
            }
        }
    }
    None
}

/// The speed cell to show for `selection` (FR-004): the Vulkan row of the
/// selected quant on [`SPEED_MACHINE_LABEL`], or `None`.
///
/// The CPU value is never substituted — it is shown in the card's detail.
pub fn speed(selection: Selection) -> Option<&'static Speed> {
    selection
        .variant
        .speed
        .iter()
        .find(|row| row.quant == selection.artifact.quant && row.backend == "vulkan")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A variant that serves German only, so the warning text and the bare-code
    /// fallback (`de-DE` → `de`) are testable: no snapshot variant is
    /// English-free.
    const GERMAN_ONLY: Variant = Variant {
        id: "german-only",
        label: "German Only",
        note: "test fixture",
        family: "test",
        license: "test",
        params: 1_000_000,
        languages: &["de"],
        downloads: &[Artifact {
            id: "german-only-Q8_0",
            quant: "Q8_0",
            file_name: "german-only-Q8_0.gguf",
            url: "https://example.invalid/german-only-Q8_0.gguf",
            size_bytes: 1,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        }],
        recommended: "Q8_0",
        accuracy: &[],
        speed: &[Speed {
            backend: "cpu",
            quant: "Q8_0",
            xrt: 1.50,
        }],
        headline: ("test", "test", "de"),
    };

    /// A variant that advertises the language in only one spelling: a configured
    /// `en-US` is then served (same language, so no warning) but has no
    /// advertised spelling to send, the one combination where the engine runs
    /// without a hint although the cards look fine.
    const EN_GB_ONLY: Variant = Variant {
        id: "en-gb-only",
        label: "English (UK) Only",
        note: "test fixture",
        family: "test",
        license: "test",
        params: 1_000_000,
        languages: &["en-GB"],
        downloads: &[Artifact {
            id: "en-gb-only-Q8_0",
            quant: "Q8_0",
            file_name: "en-gb-only-Q8_0.gguf",
            url: "https://example.invalid/en-gb-only-Q8_0.gguf",
            size_bytes: 1,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        }],
        recommended: "Q8_0",
        accuracy: &[],
        speed: &[],
        headline: ("test", "test", "en"),
    };

    /// The three artifacts the app shipped before the snapshot existed; their
    /// ids, sizes and digests are a compatibility contract (SC-006).
    const LEGACY: [(&str, u64, &str); 3] = [
        (
            "nemotron-3.5-asr-streaming-0.6b-Q8_0",
            751_094_240,
            "b94545b313b3223fda7b2857a52681da813935c2127643d1e9ff0c23d988089c",
        ),
        (
            "nemotron-3.5-asr-streaming-0.6b-Q4_K_M",
            495_831_520,
            "41c99fa5fb6f3d35f68e79adc3e755eca2232a8d921178bd647b71194792b8fd",
        ),
        (
            "nemotron-speech-streaming-en-0.6b-Q8_0",
            729_650_176,
            "90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110",
        ),
    ];

    #[test]
    fn snapshot_offers_the_eight_streaming_variants() {
        assert_eq!(VARIANTS.len(), 8);
        assert_eq!(
            VARIANTS.map(|variant| variant.id),
            [
                "moonshine-streaming-medium",
                "moonshine-streaming-small",
                "moonshine-streaming-tiny",
                "multitalker-parakeet-streaming-0.6b-v1",
                "nemotron-3.5-asr-streaming-0.6b",
                "nemotron-speech-streaming-en-0.6b",
                "parakeet-unified-en-0.6b",
                "voxtral-mini-4b-realtime-2602",
            ]
        );

        for variant in &VARIANTS {
            assert!(
                !variant.downloads.is_empty(),
                "{} must offer at least one artifact",
                variant.id
            );
            let quant = variant.recommended;
            assert!(
                variant.downloads.iter().any(|a| a.quant == quant),
                "{} must offer the recommended quant {quant}",
                variant.id
            );
        }

        // FR-002 pins the shipped default at Q8_0, whatever a card preselects.
        assert_eq!(default_selection().artifact.id, DEFAULT_ARTIFACT_ID);
        assert_eq!(default_selection().artifact.quant, "Q8_0");
        // Ascending by label is the snapshot order and the default sort.
        let mut labels: Vec<&str> = VARIANTS.iter().map(|variant| variant.label).collect();
        let sorted = {
            let mut sorted = labels.clone();
            sorted.sort_unstable();
            sorted
        };
        assert_eq!(labels, sorted, "the snapshot order is the name order");
        labels.dedup();
        assert_eq!(labels.len(), VARIANTS.len(), "labels must be unique");
    }

    #[test]
    fn every_artifact_id_and_file_name_is_unique() {
        let all: Vec<Selection> = artifacts().collect();
        // 3 Moonshine + 3×4 quantised variants + 2 = 23 offered files.
        assert_eq!(all.len(), 23);

        let mut ids: Vec<&str> = all.iter().map(|s| s.artifact.id).collect();
        let mut names: Vec<&str> = all.iter().map(|s| s.artifact.file_name).collect();
        ids.sort_unstable();
        names.sort_unstable();
        ids.dedup();
        names.dedup();
        assert_eq!(ids.len(), all.len(), "artifact ids must be unique");
        assert_eq!(names.len(), all.len(), "file names must be unique");

        for selection in &all {
            assert_eq!(
                selection.artifact.id,
                format!("{}-{}", selection.variant.id, selection.artifact.quant)
            );
            assert!(
                selection.artifact.file_name.ends_with(".gguf"),
                "{} must name a GGUF file",
                selection.artifact.file_name
            );
            assert_eq!(
                artifact(selection.artifact.id),
                Some(*selection),
                "every id must resolve back to its own selection"
            );
            assert_eq!(
                selection_for_file_name(selection.artifact.file_name),
                Some(*selection),
                "every file name must resolve to its own selection"
            );
        }
    }

    #[test]
    fn legacy_artifact_contract_is_unchanged() {
        for (id, size, sha) in LEGACY {
            let selection = artifact(id).expect("the legacy artifacts stay in the snapshot");
            assert_eq!(selection.artifact.size_bytes, size, "{id}");
            assert_eq!(selection.artifact.sha256, sha, "{id}");
            assert_eq!(selection.artifact.file_name, format!("{id}.gguf"));
        }
        assert!(
            ONBOARDING_ARTIFACT_IDS
                .iter()
                .all(|id| artifact(id).is_some())
        );
    }

    #[test]
    fn artifact_urls_are_pinned_and_name_their_file() {
        for selection in artifacts() {
            let url = selection.artifact.url;
            assert!(
                url.starts_with("https://huggingface.co/handy-computer/"),
                "{url} must point at the publishing repo"
            );
            let (prefix, tail) = url
                .split_once("/resolve/")
                .unwrap_or_else(|| panic!("{url} must be pinned to a revision"));
            assert!(
                !prefix.ends_with("/main"),
                "{url} must not point at a moving branch"
            );
            let mut parts = tail.splitn(2, '/');
            let revision = parts.next().unwrap_or_default();
            let path = parts.next().unwrap_or_default();
            assert_eq!(revision.len(), 40, "{url} must pin a full commit revision");
            assert!(
                revision
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()),
                "{url} must pin a hex revision"
            );
            assert!(
                path.ends_with(selection.artifact.file_name),
                "{url} must end in the artifact file name"
            );

            assert_eq!(selection.artifact.sha256.len(), 64);
            assert!(
                selection
                    .artifact
                    .sha256
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()),
                "{} must carry a hex digest",
                selection.artifact.id
            );
            assert!(selection.artifact.size_bytes > 0);
        }
    }

    #[test]
    fn advertised_languages_drive_the_hint_spelling() {
        let nemotron =
            artifact("nemotron-3.5-asr-streaming-0.6b-Q8_0").expect("the default is offered");
        assert_eq!(
            language_hint(nemotron.variant, Language::new("de-DE")),
            Some("de-DE")
        );
        assert_eq!(
            language_hint(nemotron.variant, Language::new("en-US")),
            Some("en-US")
        );
        assert_eq!(language_hint(nemotron.variant, Language::AUTO), None);

        // Voxtral advertises bare language codes, so the hint must be bare too.
        let voxtral = artifact("voxtral-mini-4b-realtime-2602-Q4_K_M").expect("offered");
        assert_eq!(
            language_hint(voxtral.variant, Language::new("de-DE")),
            Some("de")
        );
        assert_eq!(
            language_hint(voxtral.variant, Language::new("en-US")),
            Some("en")
        );
        assert_eq!(
            language_hint(voxtral.variant, Language::new("fr-CA")),
            Some("fr"),
            "a locale the variant does not list falls back to its bare language"
        );
        assert_eq!(
            language_hint(voxtral.variant, Language::new("vi-VN")),
            None,
            "a language the variant does not serve runs without a hint"
        );

        // The English-only variants advertise exactly `["en"]`.
        let moonshine = artifact("moonshine-streaming-tiny-Q8_0").expect("offered");
        assert_eq!(
            language_hint(moonshine.variant, Language::new("en-US")),
            Some("en")
        );
        assert_eq!(
            language_hint(moonshine.variant, Language::new("de-DE")),
            None
        );
        assert_eq!(language_hint(moonshine.variant, Language::AUTO), None);

        // A variant that lists only `en-GB` serves `en-US` (same language, so no
        // warning) but has no spelling to send: the engine runs without a hint.
        assert!(supports(&EN_GB_ONLY, Language::new("en-US")));
        assert_eq!(language_hint(&EN_GB_ONLY, Language::new("en-US")), None);
        assert_eq!(
            language_hint(&EN_GB_ONLY, Language::new("en-GB")),
            Some("en-GB")
        );

        // Every hint is byte-equal to an advertised entry (the C API's rule).
        for selection in artifacts() {
            for language in Language::ALL {
                if let Some(hint) = language_hint(selection.variant, language) {
                    assert!(
                        selection.variant.languages.contains(&hint),
                        "{} must advertise the hint {hint}",
                        selection.variant.id
                    );
                }
            }
        }
    }

    #[test]
    fn unsupported_language_warns_but_stays_selectable() {
        let moonshine = artifact("moonshine-streaming-tiny-Q8_0").expect("offered");
        assert_eq!(
            language_warning(moonshine.variant, Language::new("de-DE")),
            Some(
                "The selected model cannot transcribe German — choose a different model."
                    .to_owned()
            )
        );
        assert_eq!(language_warning(moonshine.variant, Language::AUTO), None);
        // FR-011: the warning never removes the artifact from the list.
        assert!(artifact("moonshine-streaming-tiny-Q8_0").is_some());

        // No snapshot variant is English-free, so the matching warning is pinned
        // through a synthetic variant rather than left untested. The text names
        // the configured language, so it stays correct for all 32 of them.
        assert_eq!(
            language_warning(&GERMAN_ONLY, Language::new("en-US")),
            Some(
                "The selected model cannot transcribe English (US) — choose a different model."
                    .to_owned()
            )
        );
        assert_eq!(
            language_warning(&GERMAN_ONLY, Language::new("en-GB")),
            Some(
                "The selected model cannot transcribe English (UK) — choose a different model."
                    .to_owned()
            )
        );
        assert_eq!(language_warning(&GERMAN_ONLY, Language::new("de-DE")), None);
        assert_eq!(
            language_hint(&GERMAN_ONLY, Language::new("de-DE")),
            Some("de")
        );

        // The warning names whatever language the config holds, so the rule is
        // pinned across the whole table.
        for language in Language::ALL {
            let warning = language_warning(&GERMAN_ONLY, language);
            if language.is_auto() || language == Language::new("de-DE") {
                assert_eq!(warning, None, "{language:?}");
            } else {
                assert!(
                    warning.is_some_and(|text| text.contains(language.display_name())),
                    "the warning must name {}",
                    language.display_name()
                );
            }
        }

        // Nemotron 3.5 serves every language the table offers.
        let nemotron = artifact("nemotron-3.5-asr-streaming-0.6b-Q8_0").expect("offered");
        for language in Language::ALL {
            assert_eq!(language_warning(nemotron.variant, language), None);
        }
    }

    #[test]
    fn the_language_table_covers_every_advertised_language() {
        for variant in &VARIANTS {
            for advertised in variant.languages {
                let bare = advertised
                    .split_once('-')
                    .map_or(*advertised, |(bare, _)| bare);
                assert!(
                    Language::ALL
                        .iter()
                        .any(|language| language.codes().is_some_and(|(_, entry)| entry == bare)),
                    "{} advertises {advertised}, which the language table does not offer",
                    variant.id
                );
            }
        }
    }

    #[test]
    fn quality_falls_back_language_then_quant() {
        fn quality_of(
            selection: Selection,
            language: Language,
        ) -> Option<(f32, &'static Accuracy)> {
            quality(selection, language).map(|found| (found.err_pct, found))
        }

        // The selected quant has no German cell, but the headline dataset does.
        let voxtral = artifact("voxtral-mini-4b-realtime-2602-Q4_K_M").expect("offered");
        let (err, found) =
            quality_of(voxtral, Language::new("de-DE")).expect("a German cell exists");
        assert_eq!(err, 4.24);
        assert_eq!(
            (found.dataset, found.split, found.quant),
            ("fleurs", "test", "Q8_0")
        );
        assert_eq!(found.metric, "wer");

        // The headline dataset wins over the other published one.
        let nemotron = artifact("nemotron-3.5-asr-streaming-0.6b-Q4_K_M").expect("offered");
        let (err, found) =
            quality_of(nemotron, Language::new("en-US")).expect("an English cell exists");
        assert_eq!(err, 3.30);
        assert_eq!((found.dataset, found.split), ("librispeech", "test-clean"));
        assert_eq!(found.quant, "Q4_K_M");

        // A locale without cells of its own falls back to its language's cells,
        // exactly like the hint falls back to the bare language.
        assert_eq!(
            quality_of(nemotron, Language::new("en-GB")).map(|(err, _)| err),
            Some(3.30)
        );
        assert_eq!(
            quality_of(nemotron, Language::new("es-ES")).map(|(err, found)| (err, found.language)),
            Some((3.30, "en")),
            "a language without a published cell falls back to the headline one"
        );

        let moonshine = artifact("moonshine-streaming-tiny-Q8_0").expect("offered");
        assert_eq!(
            quality_of(moonshine, Language::new("en-US")).map(|(err, _)| err),
            Some(4.52)
        );
        // `auto` uses the headline cell (librispeech for Moonshine, fleurs for Nemotron).
        assert_eq!(
            quality_of(moonshine, Language::AUTO).map(|(err, _)| err),
            Some(4.52)
        );
        assert_eq!(
            quality_of(nemotron, Language::AUTO).map(|(err, found)| (err, found.dataset)),
            Some((3.30, "librispeech"))
        );
    }

    #[test]
    fn quality_ignores_measurements_of_unoffered_quants_and_other_metrics() {
        // The snapshot carries only rows of offered quants: an F16 row would be
        // displayed for a selection the user cannot make.
        for selection in artifacts() {
            let variant = selection.variant;
            for cell in variant.accuracy {
                assert!(
                    variant
                        .downloads
                        .iter()
                        .any(|artifact| artifact.quant == cell.quant),
                    "{}: {cell:?} belongs to an unoffered quant",
                    variant.id
                );
                assert!(
                    matches!(cell.metric, "wer" | "cer"),
                    "{}: {cell:?} is neither WER nor CER",
                    variant.id
                );
                assert!(
                    variant.languages.contains(&cell.language)
                        || ["en", "de"].contains(&cell.language),
                    "{}: {cell:?} names an unknown language",
                    variant.id
                );
            }
            assert!(quality(selection, Language::AUTO).is_some());
        }
    }

    #[test]
    fn speed_uses_the_vulkan_row_and_never_the_cpu_row() {
        let parakeet = artifact("parakeet-unified-en-0.6b-Q8_0").expect("offered");
        let row = speed(parakeet).expect("a Vulkan row exists");
        assert_eq!((row.backend, row.xrt), ("vulkan", 26.01));

        // Nemotron 3.5 Q6_K has no published speed row at all: the card shows
        // `—`, never a nearby quant's number (FR-004).
        let nemotron = artifact("nemotron-3.5-asr-streaming-0.6b-Q6_K").expect("offered");
        assert_eq!(speed(nemotron), None);

        // A quant measured on the CPU only must not report the CPU value: the
        // synthetic fixture carries exactly that shape, because the snapshot's
        // two measured quants are measured on both backends.
        let cpu_only = Selection {
            variant: &GERMAN_ONLY,
            artifact: &GERMAN_ONLY.downloads[0],
        };
        assert_eq!(speed(cpu_only), None);
        assert!(
            cpu_only.variant.speed.iter().any(|row| row.quant == "Q8_0"),
            "the fixture has a CPU row, so only the backend can be the reason"
        );

        for selection in artifacts() {
            assert!(speed(selection).is_none_or(|row| row.backend == "vulkan"));
            assert!(
                speed(selection).is_none_or(|row| row.quant == selection.artifact.quant),
                "{} must show its own quant",
                selection.artifact.id
            );
        }
    }

    #[test]
    fn params_and_language_labels() {
        assert_eq!(params_label(637_991_968), "638 M");
        assert_eq!(params_label(44_054_721), "44 M");
        assert_eq!(params_label(4_429_707_024), "4.4 B");
        assert_eq!(params_label(1_000_000_000), "1.0 B");

        let nemotron = artifact("nemotron-3.5-asr-streaming-0.6b-Q8_0").expect("offered");
        assert_eq!(languages_label(nemotron.variant), "32 languages");
        let moonshine = artifact("moonshine-streaming-tiny-Q8_0").expect("offered");
        assert_eq!(languages_label(moonshine.variant), "1 language");
        let voxtral = artifact("voxtral-mini-4b-realtime-2602-Q8_0").expect("offered");
        assert_eq!(languages_label(voxtral.variant), "13 languages");
    }

    #[test]
    fn unknown_ids_resolve_to_the_default() {
        assert_eq!(artifact("nope"), None);
        assert_eq!(artifact(""), None);
        assert_eq!(artifact("nemotron-3.5-asr-streaming-0.6b"), None);
        assert_eq!(artifact("nemotron-3.5-asr-streaming-0.6b-Q8_0-X"), None);
        assert_eq!(artifact("nemotron-3.5-asr-streaming-0.6b-F16"), None);
        assert_eq!(
            artifact(DEFAULT_ARTIFACT_ID).map(|s| s.artifact.id),
            Some(DEFAULT_ARTIFACT_ID)
        );
        assert_eq!(default_selection().artifact.id, DEFAULT_ARTIFACT_ID);
        assert_eq!(
            selection_for_file_name("not-a-lingua-model.gguf"),
            None,
            "a foreign file never resolves"
        );
        // Multitalker's upstream path has a `bundle/` prefix; on disk the file
        // name is flat (the model directory is flat).
        let multitalker = artifact("multitalker-parakeet-streaming-0.6b-v1-Q8_0").expect("offered");
        assert!(!multitalker.artifact.file_name.contains('/'));
        assert!(multitalker.artifact.url.contains("/bundle/"));
    }

    #[test]
    fn the_recommendation_is_the_smallest_measured_quant_inside_the_noise_band() {
        /// The band the catalog's [`Recommendation`] rule names: every published
        /// quant cell is a single measurement, so less than this cannot be
        /// resolved.
        const NOISE_BAND_PP: f32 = 0.05;

        // `Some(true)`: measured on both axes — inside the error band on every
        // cell Q8_0 was measured on, and a speed row. `Some(false)`: measured,
        // but outside the band. `None`: nothing published for it.
        let qualifies = |variant: &Variant, quant: &str| -> Option<bool> {
            let mut measured = false;
            for row in variant.accuracy.iter().filter(|row| row.quant == quant) {
                let Some(base) = variant.accuracy.iter().find(|other| {
                    other.quant == "Q8_0"
                        && other.dataset == row.dataset
                        && other.split == row.split
                        && other.language == row.language
                }) else {
                    continue;
                };
                measured = true;
                if row.err_pct - base.err_pct > NOISE_BAND_PP {
                    return Some(false);
                }
            }
            if !measured {
                return None;
            }

            // A quant upstream published no throughput for is not a preset: its
            // card would draw no speed bar and the speed sort could not rank it.
            let rated = variant.speed.iter().any(|row| row.quant == quant);

            rated.then_some(true)
        };

        for variant in &VARIANTS {
            let pick = recommended(variant);
            assert_eq!(
                qualifies(variant, pick.quant),
                Some(true),
                "{}: {} is recommended without being measured on both axes",
                variant.id,
                pick.quant
            );

            // Nothing smaller may qualify: this is what makes the pick the
            // smallest one, and it is the tripwire for new upstream numbers — a
            // quant that moves inside the band fails here and forces the decision
            // to be taken again instead of drifting.
            for smaller in variant
                .downloads
                .iter()
                .filter(|artifact| artifact.size_bytes < pick.size_bytes)
            {
                assert_ne!(
                    qualifies(variant, smaller.quant),
                    Some(true),
                    "{}: {} is smaller and qualifies, so it, not {}, is the recommendation",
                    variant.id,
                    smaller.quant,
                    pick.quant
                );
            }
        }
    }

    #[test]
    fn selection_label_names_variant_and_quant() {
        let selection = default_selection();
        assert_eq!(selection.label(), "Nemotron 3.5 ASR Streaming 0.6B — Q8_0");
    }
}
