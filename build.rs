use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile = env::var("PROFILE").unwrap_or_default();

    // Build dctags static library from amalgamated source
    // Single amalgamated file instead of 16 individual C files
    let mut cc = cc::Build::new();

    // Use clang for cross-language LTO compatibility with Rust's LLVM
    cc.compiler("clang");

    cc.include("src/ctags")
      .files(&[
          // FFI wrapper
          "src/wrapper.c",
          // Amalgamated ctags source (core infrastructure)
          "src/ctags/ctags_amalg.c",
      ])
      .define("HAVE_CONFIG_H", None)
      .define("HAVE_STDLIB_H", None)
      .define("HAVE_UNISTD_H", None)
      .define("HAVE_OPENDIR", None)
      .define("HAVE_FNMATCH", None)
      .define("HAVE_STRSTR", None)
      .define("HAVE_STRCASECMP", None)
      .define("HAVE_STRERROR", None)
      .define("HAVE_SYS_TYPES_H", None)
      .define("HAVE_FCNTL_H", None)
      .define("HAVE_SYS_STAT_H", None)
      .define("HAVE_TIME_H", None)
      .define("HAVE_CLOCK", None)
      .define("HAVE_DIRENT_H", None)
      .define("HAVE_FNMATCH_H", None)
      .define("DEBUG", None)
      .define("PROFILE_CTAGS", None)
      .define("PRECC_FAST_PATH", None)  // Optimize debugPutc for precc
      .opt_level(3)  // Maximum optimization
      .flag("-march=native")  // Optimize for current CPU
      .warnings(true)
      .flag("-Wno-all")
      .flag("-Wno-cpp")
      .flag("-Wno-sign-compare")
      .flag("-Wno-implicit-fallthrough")
      .flag("-Wno-return-local-addr")
      .flag("-Wno-return-stack-address")
      .flag("-Wno-unknown-warning-option")
      .flag("-Wno-deprecated-declarations")
      .flag("-Wno-missing-declarations")
      .flag("-Wno-macro-redefined")
      .flag("-Wno-builtin-declaration-mismatch")
      .flag("-Wno-unused-parameter")
      .flag("-Wno-warn_unused_result");

    // Enable cross-language LTO for release builds
    if profile == "release" {
        cc.flag("-flto=thin");
    }

    cc.compile("dctags");

    // Tell cargo where to find the library
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=dctags");

    // Rebuild if any of these files change
    println!("cargo:rerun-if-changed=src/wrapper.c");
    println!("cargo:rerun-if-changed=src/ctags/ctags_amalg.c");
    println!("cargo:rerun-if-changed=src/ctags/precc_ffi.h");

    // Bake git revision into the binary at compile time.
    // Falls back to "unknown" so the build never fails in a non-git context.
    let git_rev = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=PRECC_GIT_REV={}", git_rev);
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}
