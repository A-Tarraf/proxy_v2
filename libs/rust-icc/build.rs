extern crate bindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    let admireic: pkg_config::Library = pkg_config::Config::new()
        .statik(false)
        .probe("admireic")
        .expect("pkg-config could not find admireic");
    let admireic_includes: Vec<String> = admireic
        .include_paths
        .iter()
        .map(|path: &PathBuf| String::from("-I") + path.to_str().or(Some("")).unwrap())
        .collect();

    // Tell cargo to tell rustc to link the admireic shared library.
    println!("cargo:rustc-link-lib=icc");

    println!("cargo:rerun-if-changed=wrapper.h");

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // The input header we would like to generate
        // bindings for.
        .header("wrapper.h")
        //  .clang_arg("-I/projects/spack/opt/spack/linux-ubuntu22.04-sandybridge/gcc-11.2.0/mochi-admireic-0.11.1-ejoccf7pulgsgcbsrpi4aeif75n45gjp/include")
        .clang_args(admireic_includes.iter())
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed.
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
