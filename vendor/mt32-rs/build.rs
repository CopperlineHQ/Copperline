// SPDX-License-Identifier: LGPL-2.1-or-later

//! With the `oracle` feature, compiles the reference C++ engine
//! (`oracle/munt`) plus a small shim so the differential tests can run this
//! crate and Munt side by side on identical input. A normal build does
//! nothing: the crate itself is pure Rust.

fn main() {
    build_oracle();
}

/// Nothing to do: the crate is pure Rust, and `cc` is not even a
/// dependency without the feature.
#[cfg(not(feature = "oracle"))]
fn build_oracle() {}

#[cfg(feature = "oracle")]
fn build_oracle() {
    let dir = std::path::Path::new("oracle/munt/src");
    // Upstream's own library source list, minus the optional resampler
    // adapters: MT32EMU_WITH_INTERNAL_RESAMPLER selects the built-in one.
    const SOURCES: [&str; 29] = [
        "Analog.cpp",
        "BReverbModel.cpp",
        "Display.cpp",
        "File.cpp",
        "FileStream.cpp",
        "LA32FloatWaveGenerator.cpp",
        "LA32Ramp.cpp",
        "LA32WaveGenerator.cpp",
        "MidiStreamParser.cpp",
        "Part.cpp",
        "Partial.cpp",
        "PartialManager.cpp",
        "Poly.cpp",
        "ROMInfo.cpp",
        "SampleRateConverter.cpp",
        "Synth.cpp",
        "Tables.cpp",
        "TVA.cpp",
        "TVF.cpp",
        "TVP.cpp",
        "VersionTagging.cpp",
        "c_interface/c_interface.cpp",
        "sha1/sha1.cpp",
        "srchelper/InternalResampler.cpp",
        "srchelper/srctools/src/FIRResampler.cpp",
        "srchelper/srctools/src/IIR2xResampler.cpp",
        "srchelper/srctools/src/LinearResampler.cpp",
        "srchelper/srctools/src/ResamplerModel.cpp",
        "srchelper/srctools/src/SincResampler.cpp",
    ];
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++11")
        .include(dir)
        .define("MT32EMU_WITH_INTERNAL_RESAMPLER", None)
        // The engine jitters its pitch-envelope timer with libc `rand()`
        // (TVP.cpp), whose sequence is process-global and differs by
        // platform -- either alone would sink "the same input renders the
        // same output". Rewriting the name points the one call site (and
        // the <cstdlib> prototype it sees) at the shim's own generator,
        // which the Rust engine mirrors exactly. The sources stay
        // byte-identical to upstream.
        .define("rand", "mt32_oracle_rand");
    // The accurate analogue filter is float arithmetic; forbid the
    // compiler from contracting its multiply-adds into fused ones, so
    // the reference computes the same low bits the port does.
    build.flag_if_supported("-ffp-contract=off");
    for source in SOURCES {
        build.file(dir.join(source));
    }
    println!("cargo:rerun-if-changed=oracle/munt/src");
    println!("cargo:rerun-if-changed=oracle/shim.cpp");
    build.file("oracle/shim.cpp");
    build.compile("mt32emu_oracle");
}
