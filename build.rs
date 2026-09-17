use std::{env, path::PathBuf, process::Command};

fn main() {
    for path in ["build.rs", "scripts/build-tinycc.sh"] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=PATH");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let result = Command::new("sh")
        .arg("scripts/build-tinycc.sh")
        .arg(out)
        .output()
        .expect("TinyCC build requires sh, Git, Perl and LLVM 19 tools in PATH");
    if !result.status.success() {
        panic!(
            "building embedded TinyCC failed; install LLVM 19 and put its bin directory in PATH:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
