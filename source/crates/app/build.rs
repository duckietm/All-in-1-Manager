//! The exe icon and version information on Windows, and the client images of `Assets` packed into the
//! binary (Deploy Assets → Images uploads the ones the server is missing), so the program is one file. An `Assets`
//! folder next to the exe is still read too, for images of your own.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=resources/app.rc");
        println!("cargo:rerun-if-changed=resources/app.ico");
        embed_resource::compile("resources/app.rc", embed_resource::NONE).manifest_optional().unwrap();
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let assets = manifest.join("../../Assets");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ClientAssets.zip");
    println!("cargo:rerun-if-changed={}", assets.display());

    let mut zip = zip::ZipWriter::new(File::create(&out).unwrap());
    // Images are compressed already.
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    if assets.is_dir() {
        add_dir(&mut zip, &assets, &assets, options);
    } else {
        println!("cargo:warning=Assets not found; the binary will carry no client images.");
    }
    zip.finish().unwrap();
}

fn add_dir(zip: &mut zip::ZipWriter<File>, base: &Path, dir: &Path, options: zip::write::SimpleFileOptions) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            add_dir(zip, base, &path, options);
            continue;
        }
        let relative = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
        let mut bytes = Vec::new();
        File::open(&path).unwrap().read_to_end(&mut bytes).unwrap();
        zip.start_file(relative, options).unwrap();
        zip.write_all(&bytes).unwrap();
    }
}
