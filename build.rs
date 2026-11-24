// Builds Yacht's synthesizer: a small C host (src/launcher/apps/yacht_synth.c) around the
// Felucca-Engines (third_party/felucca-engines, MIT), compiled as one translation unit.

fn main() {
    let eng = "third_party/felucca-engines";
    println!("cargo:rerun-if-changed=src/launcher/apps/yacht_synth.c");
    println!("cargo:rerun-if-changed={eng}");
    let mut b = cc::Build::new();
    // The web build: cc picks the host clang, which has no C library for this target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("emscripten") {
        b.compiler("emcc");
    }
    b.file("src/launcher/apps/yacht_synth.c")
        .include(eng)
        .std("c99")
        .opt_level(2)
        .warnings(false)
        .compile("yacht_synth");
}
