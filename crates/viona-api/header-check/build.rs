#![deny(warnings)]

use std::convert::TryFrom;
use std::env;
use std::path::PathBuf;

fn main() {
    let mut cfg = ctest2::TestGenerator::new();

    let gate_dir = match env::var("GATE_SRC").map(PathBuf::try_from) {
        Ok(Ok(dir)) => dir,
        _ => {
            eprintln!("Must specify path to illumos-gate sources with GATE_SRC env var");
            std::process::exit(1);
        }
    };

    let include_paths = [
        "usr/src/uts/intel",
        "usr/src/uts/common",
    ];
    cfg.include("/usr/include");
    for p in include_paths {
        cfg.include(gate_dir.join(p));
    }

    cfg.header("sys/types.h");
    cfg.header("sys/viona_io.h");

    cfg.skip_const(move |name| match name {
        "VIONA_DEV_PATH" => true,

        _ => false,
    });

    cfg.skip_field(move |name, field| match (name, field) {
        // C header currently lacks explicit pad fields
        ("vioc_ring_init", "_pad") => true,
        ("vioc_ring_msi", "_pad") => true,

        _ => false,
    });

    cfg.skip_roundtrip(move |name| match name {
        // lack of explicit padding causes round-trip problems
        "vioc_ring_init" => true,
        "vioc_ring_msi" => true,

        _ => false,
    });

    cfg.generate("../sys/src/lib.rs", "main.rs");
}
