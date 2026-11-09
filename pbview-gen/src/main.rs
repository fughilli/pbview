//! `pbview-gen <descriptor_set.bin> <out> [--crate-root] [--runtime PATH] [--lang rust|cpp] [--bounds FILE]`
//!
//! Reads a protoc `--descriptor_set_out` FileDescriptorSet and writes the
//! zero-copy view module (Rust, default) or header (`--lang cpp`; `--runtime`
//! is then the `#include` path of pbview.h) for every file in it.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pos = Vec::new();
    let mut opts = pbview_gen::Options::default();
    let mut lang = "rust".to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--crate-root" => opts.crate_root = true,
            "--runtime" => opts.runtime = it.next().cloned(),
            "--lang" => lang = it.next().cloned().unwrap_or_default(),
            "--bounds" => {
                let path = it.next().cloned().unwrap_or_default();
                let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    eprintln!("pbview-gen: read {path}: {e}");
                    std::process::exit(1)
                });
                opts.bounds = pbview_gen::parse_bounds(&text).unwrap_or_else(|e| {
                    eprintln!("pbview-gen: {path}: {e}");
                    std::process::exit(1)
                });
            }
            _ => pos.push(a.clone()),
        }
    }
    if pos.len() != 2 {
        eprintln!("usage: pbview-gen <descriptor_set.bin> <out.rs> [--crate-root] [--runtime PATH] [--lang rust|cpp] [--bounds FILE]");
        std::process::exit(2);
    }
    let fdset = std::fs::read(&pos[0]).unwrap_or_else(|e| {
        eprintln!("pbview-gen: read {}: {e}", pos[0]);
        std::process::exit(1)
    });
    let generated = match lang.as_str() {
        "rust" => pbview_gen::generate(&fdset, &opts),
        "cpp" => {
            let mut c = pbview_gen::cpp::CppOptions::default();
            if let Some(r) = &opts.runtime {
                c.runtime_include = r.clone();
            }
            pbview_gen::cpp::generate(&fdset, &c)
        }
        other => Err(format!("unknown --lang {other} (rust or cpp)")),
    };
    match generated {
        Ok(code) => {
            if let Err(e) = std::fs::write(&pos[1], code) {
                eprintln!("pbview-gen: write {}: {e}", pos[1]);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("pbview-gen: {e}");
            std::process::exit(1);
        }
    }
}
