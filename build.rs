//! Link fix-up for the vendored transcribe.cpp build.
//!
//! transcribe.cpp's CMake configures its host decoder against `cblas.h` when a
//! CBLAS header is present (`TRANSCRIBE_HAS_BLAS`, see its `CMakeLists.txt`),
//! but the link manifest that `transcribe-cpp-sys` consumes only names
//! `libblas` — `libcblas` is never emitted. On distributions where those are two
//! separate libraries (Arch: `blas` + `cblas`) the final link then fails with
//! undefined `cblas_sgemm`/`cblas_sgemv` symbols, which this script prevents by
//! reproducing the same header probe and asking rustc for the library the
//! compiled code actually calls.
//!
//! When no CBLAS header exists the C++ side does not call CBLAS either, so
//! nothing is emitted and `libblas` alone stays correct.

use std::path::Path;

/// The include locations the C++ configure step can find (mirrors a plain
/// `find_package(BLAS)` + `check_include_file(cblas.h)` probe).
const CBLAS_HEADERS: [&str; 3] = [
    "/usr/include/cblas.h",
    "/usr/local/include/cblas.h",
    "/usr/include/openblas/cblas.h",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }

    if std::env::var_os("CBLAS_NO_LINK").is_some() {
        return;
    }

    if CBLAS_HEADERS
        .iter()
        .any(|header| Path::new(header).exists())
    {
        println!("cargo:rustc-link-lib=dylib=cblas");
    }
}
