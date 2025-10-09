use std::env;
use std::path::PathBuf;

fn main() {
    let mut libccp_make = std::process::Command::new("make");
    libccp_make.arg("libccp.a");

    if cfg!(feature = "log_trace") {
        libccp_make.arg("LOGLEVEL=trace");
    } else if cfg!(feature = "log_debug") {
        libccp_make.arg("LOGLEVEL=debug");
    } else if cfg!(feature = "log_info") {
        libccp_make.arg("LOGLEVEL=info");
    } else if cfg!(feature = "log_warn") {
        libccp_make.arg("LOGLEVEL=warn");
    } else if cfg!(feature = "log_error") {
        libccp_make.arg("LOGLEVEL=error");
    }

    libccp_make.current_dir("./libccp");
    let mut libccp_make = libccp_make.spawn().expect("libccp make failed");
    libccp_make.wait().expect("libccp make spawned but failed");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    std::process::Command::new("mv")
        .arg("./libccp/libccp.a")
        .arg(out_path.join("libccp.a"))
        .spawn()
        .expect("mv static library failed")
        .wait()
        .expect("mv spawned but failed");

    std::process::Command::new("make")
        .arg("clean")
        .current_dir("./libccp")
        .spawn()
        .expect("libccp make clean failed")
        .wait()
        .expect("libccp make clean spawned but failed");

    println!(
        "cargo:rustc-link-search={}",
        out_path.to_str().expect("OUT_DIR error")
    );
    println!("cargo:rustc-link-lib=static=ccp");

    let bindings = bindgen::Builder::default()
        .header("./libccp/ccp.h")
        .allowlist_function(r#"ccp_\w+"#)
        .allowlist_var(r#"LIBCCP_\w+"#)
        .blocklist_type(r#"u\d+"#)
        .generate()
        .expect("Unable to generate bindings");

    bindings
        .write_to_file(out_path.join("libccp.rs"))
        .expect("Unable to write bindings");
}
