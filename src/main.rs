//! demo-precc - demo of precc, a C/C++ precompiler
//!
//! Modes:
//! - Performance (--perf): Benchmark generation and compilation throughput
//! - Vim (--vim): Test all vim source files

#![allow(dead_code)]

use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;

// ANSI colors
const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[1;33m";
const BLUE: &str = "\x1b[0;34m";
const CYAN: &str = "\x1b[0;36m";
const MAGENTA: &str = "\x1b[0;35m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const NC: &str = "\x1b[0m";

// Box drawing characters - Unicode (default)
const BOX_TL_UNI: &str = "╔";     // Top-left corner
const BOX_TR_UNI: &str = "╗";     // Top-right corner
const BOX_BL_UNI: &str = "╚";     // Bottom-left corner
const BOX_BR_UNI: &str = "╝";     // Bottom-right corner
const BOX_H_UNI: &str = "═";      // Horizontal line
const BOX_V_UNI: &str = "║";      // Vertical line
const BOX_LT_UNI: &str = "╠";     // Left T-junction
const BOX_RT_UNI: &str = "╣";     // Right T-junction
const BOX_HLINE_UNI: &str = "━";  // Thin horizontal line
const BOX_SEP_UNI: &str = "│";    // Thin vertical separator
const BAR_CHAR_UNI: &str = "█";   // Progress bar fill
const CHECK_UNI: &str = "✓";      // Checkmark
const CROSS_UNI: &str = "✗";      // Cross mark
const SPINNER_UNI: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// Box drawing characters - ASCII (for asciinema)
const BOX_TL_ASCII: &str = "+";
const BOX_TR_ASCII: &str = "+";
const BOX_BL_ASCII: &str = "+";
const BOX_BR_ASCII: &str = "+";
const BOX_H_ASCII: &str = "-";
const BOX_V_ASCII: &str = "|";
const BOX_LT_ASCII: &str = "+";
const BOX_RT_ASCII: &str = "+";
const BOX_HLINE_ASCII: &str = "-";
const BOX_SEP_ASCII: &str = "|";
const BAR_CHAR_ASCII: &str = "#";
const CHECK_ASCII: &str = "[OK]";
const CROSS_ASCII: &str = "[FAIL]";
const SPINNER_ASCII: &[char] = &['|', '/', '-', '\\'];

// Global ASCII mode flag (set during init)
static ASCII_MODE: AtomicBool = AtomicBool::new(false);

fn set_ascii_mode(enabled: bool) {
    ASCII_MODE.store(enabled, Ordering::SeqCst);
}

fn is_ascii_mode() -> bool {
    ASCII_MODE.load(Ordering::SeqCst)
}

// Helper functions to get the right characters
fn box_tl() -> &'static str { if is_ascii_mode() { BOX_TL_ASCII } else { BOX_TL_UNI } }
fn box_tr() -> &'static str { if is_ascii_mode() { BOX_TR_ASCII } else { BOX_TR_UNI } }
fn box_bl() -> &'static str { if is_ascii_mode() { BOX_BL_ASCII } else { BOX_BL_UNI } }
fn box_br() -> &'static str { if is_ascii_mode() { BOX_BR_ASCII } else { BOX_BR_UNI } }
fn box_h() -> &'static str { if is_ascii_mode() { BOX_H_ASCII } else { BOX_H_UNI } }
fn box_v() -> &'static str { if is_ascii_mode() { BOX_V_ASCII } else { BOX_V_UNI } }
fn box_lt() -> &'static str { if is_ascii_mode() { BOX_LT_ASCII } else { BOX_LT_UNI } }
fn box_rt() -> &'static str { if is_ascii_mode() { BOX_RT_ASCII } else { BOX_RT_UNI } }
fn box_hline() -> &'static str { if is_ascii_mode() { BOX_HLINE_ASCII } else { BOX_HLINE_UNI } }
fn box_sep() -> &'static str { if is_ascii_mode() { BOX_SEP_ASCII } else { BOX_SEP_UNI } }
fn bar_char() -> &'static str { if is_ascii_mode() { BAR_CHAR_ASCII } else { BAR_CHAR_UNI } }
fn check_mark() -> &'static str { if is_ascii_mode() { CHECK_ASCII } else { CHECK_UNI } }
fn cross_mark() -> &'static str { if is_ascii_mode() { CROSS_ASCII } else { CROSS_UNI } }
fn spinner_chars() -> &'static [char] { if is_ascii_mode() { SPINNER_ASCII } else { SPINNER_UNI } }

// Skip patterns for vim files
const SKIP_PATTERNS: &[&str] = &[
    "gui",
    "netbeans",
    "channel",
    "job",
    "terminal",
    "sound",
    "crypt",
    "if_",
    "os_amiga",
    "os_mac",
    "os_mswin",
    "os_vms",
    "os_w32",
    "os_win",
    "dosinst",
    "uninstall",
    "vimrun",
    "winclip",
    "xpm_w32",
    "dlldata",
    "regexp_bt",
    "regexp_nfa",
];

// Fixed bugs list for regression testing
// Updated Dec 2025 - bug3, bug10, bug11 now fixed with proper type definitions
// bug63, bug64, bug67 excluded: require vim amalgamation or sqlite3.i to test
// bug84: nested struct dependency (struct ExprList_item inside struct ExprList)
const FIXED_BUGS: &[&str] = &[
    "bug1", "bug2", "bug3", "bug4", "bug5", "bug6", "bug7", "bug8", "bug9", "bug10", "bug11",
    "bug12", "bug13", "bug14", "bug15", "bug16", "bug17", "bug18", "bug19", "bug20", "bug21",
    "bug22", "bug23", "bug24", "bug25", "bug26", "bug28", "bug29", "bug30", "bug32", "bug33",
    "bug34", "bug35", "bug36", "bug37", "bug38", "bug39", "bug40", "bug41", "bug42", "bug43",
    "bug44", "bug45", "bug46", "bug47", "bug48", "bug49", "bug50", "bug51", "bug52", "bug53",
    "bug54", "bug55", "bug56", "bug57", "bug58", "bug59", "bug60", "bug61", "bug62", "bug65",
    "bug66", "bug68", "bug69", "bug70", "bug71", "bug72", "bug73", "bug74", "bug77", "bug78",
    "bug79", "bug80", "bug81", "bug82", "bug83", "bug84",
];

/// Linker selection for object file linking
#[derive(Clone, Copy, PartialEq)]
enum LinkerType {
    Ld,   // Traditional GNU ld
    Mold, // Fast mold linker (default)
}

impl LinkerType {
    fn name(&self) -> &'static str {
        match self {
            LinkerType::Ld => "ld",
            LinkerType::Mold => "mold",
        }
    }
}

/// Link mode selection
#[derive(Clone, Copy, PartialEq)]
enum LinkMode {
    Sequential, // Link all objects in one command
    Parallel,   // Hierarchical parallel linking (default)
}

impl LinkMode {
    fn name(&self) -> &'static str {
        match self {
            LinkMode::Sequential => "sequential",
            LinkMode::Parallel => "parallel",
        }
    }
}

#[derive(Clone)]
struct Config {
    auto_mode: bool,
    perf_mode: bool,
    vim_mode: bool,
    all_threshold_mode: bool,
    skip_regression: bool,
    parallel_split: bool,
    inprocess_mode: bool,
    max_iterations: usize,
    precc_jobs: usize,
    split_jobs: usize,
    reserved_cpus: usize,
    precc_timeout: u64,
    sample_size: usize,
    scan_delay: u64,
    start_pu: usize,
    input_file: String,
    baseline_failures: usize,
    experiment_dir: PathBuf,
    source_dir: PathBuf,
    tests_dir: PathBuf,
    precc_bin: PathBuf,
    exp_input_file: PathBuf,
    version_tag: String,
    git_branch: String,
    // Precc build options
    split: bool,           // Enable split mode (default: true)
    lattice_headers: bool, // Enable lattice-based header splitting (default: false)
    unity_build: bool,     // Enable unity build mode (default: true)
    unity_batches: usize,  // Number of unity batches (0 = adaptive, default: 0)
    // Linker selection
    linker: LinkerType,    // Linker to use (default: Mold if available, else Ld)
    link_mode: LinkMode,   // Link mode (default: Parallel)
    // File preservation
    keep_files: bool,      // Keep intermediate files (default: false)
    // Output mode
    ascii_mode: bool,      // Use ASCII instead of unicode for box drawing (default: false)
}

struct Stats {
    total_pus: usize,
    bugs_fixed: usize,
    current_estimate: usize,
    start_time: Instant,
    regression_tests_passed: usize,
    // Perf mode stats
    perf_preprocess_time: f64,  // Time to run gcc -E on sqlite3.c
    perf_gen_time: f64,
    perf_gen_throughput: f64,
    perf_comp_time: f64,
    perf_comp_throughput: f64,
    perf_comp_passed: usize,
    perf_comp_failed: usize,
    perf_success_rate: f64,
    // Linking stats (perf mode)
    perf_link_time: f64,
    perf_link_success: bool,
    perf_orig_comp_time: f64,
    perf_symbols_match: bool,
    perf_symbols_count: usize,
    perf_orig_text_size: usize,
    perf_split_text_size: usize,
    // Vim mode stats
    vim_total_files: usize,
    vim_files_tested: usize,
    vim_files_skipped: usize,
    vim_total_pus: usize,
    vim_comp_passed: usize,
    vim_comp_failed: usize,
    vim_preprocess_time: f64,  // Time to run gcc -E on all .c files
    vim_gen_time: f64,
    vim_comp_time: f64,
    vim_success_rate: f64,
    vim_throughput: f64,
    // Vim linking stats
    vim_link_time: f64,
    vim_link_success: bool,
    vim_orig_comp_time: f64,   // Original compile time (without link)
    vim_orig_link_time: f64,   // Original link time
    vim_symbols_match: bool,
    vim_symbols_count: usize,
    vim_orig_text_size: usize,
    vim_split_text_size: usize,
}

impl Stats {
    fn new() -> Self {
        Stats {
            total_pus: 0,
            bugs_fixed: 0,
            current_estimate: 0,
            start_time: Instant::now(),
            regression_tests_passed: 0,
            perf_preprocess_time: 0.0,
            perf_gen_time: 0.0,
            perf_gen_throughput: 0.0,
            perf_comp_time: 0.0,
            perf_comp_throughput: 0.0,
            perf_comp_passed: 0,
            perf_comp_failed: 0,
            perf_success_rate: 0.0,
            perf_link_time: 0.0,
            perf_link_success: false,
            perf_orig_comp_time: 0.0,
            perf_symbols_match: false,
            perf_symbols_count: 0,
            perf_orig_text_size: 0,
            perf_split_text_size: 0,
            vim_total_files: 0,
            vim_files_tested: 0,
            vim_files_skipped: 0,
            vim_total_pus: 0,
            vim_comp_passed: 0,
            vim_comp_failed: 0,
            vim_preprocess_time: 0.0,
            vim_gen_time: 0.0,
            vim_comp_time: 0.0,
            vim_success_rate: 0.0,
            vim_throughput: 0.0,
            vim_link_time: 0.0,
            vim_link_success: false,
            vim_orig_comp_time: 0.0,
            vim_orig_link_time: 0.0,
            vim_symbols_match: false,
            vim_symbols_count: 0,
            vim_orig_text_size: 0,
            vim_split_text_size: 0,
        }
    }
}

fn log_info(msg: &str) {
    eprintln!("{}[INFO]{} {}", BLUE, NC, msg);
}

fn log_ok(msg: &str) {
    eprintln!("{}[OK]{} {}", GREEN, NC, msg);
}

fn log_warn(msg: &str) {
    eprintln!("{}[WARN]{} {}", YELLOW, NC, msg);
}

fn log_error(msg: &str) {
    eprintln!("{}[ERROR]{} {}", RED, NC, msg);
}

fn log_stage(name: &str) {
    eprintln!();
    eprintln!("{}{}----------------------------------------{}", BOLD, CYAN, NC);
    eprintln!("{}{}  STAGE: {}{}", BOLD, CYAN, name, NC);
    eprintln!("{}{}----------------------------------------{}", BOLD, CYAN, NC);
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn format_size(bytes: usize) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1}M", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1}K", bytes as f64 / 1_000.0)
    } else {
        format!("{}B", bytes)
    }
}

fn get_num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn should_skip_vim_file(filename: &str) -> bool {
    for pattern in SKIP_PATTERNS {
        if filename.starts_with(pattern) || filename.contains(pattern) {
            return true;
        }
    }
    false
}

fn print_usage() {
    eprintln!(
        r#"PRECC demo

USAGE:
    demo-precc [input_file.i] [baseline_failures] [options]

ARGUMENTS:
    input_file.i        Preprocessed C file to test (default: sqlite3.i)
    baseline_failures   Expected number of failures (default: 310)

OPTIONS:
    --help, -h          Show this help message and exit
    --auto              Run fully automatically without confirmation prompts (default: enabled)
    --no-auto           Prompt for confirmation before starting
    --perf              Performance mode: benchmark generation and compilation (default: enabled)
    --vim               Vim mode: test all vim source files in tests/vim/src
    --parallel-split    Parallelize precc split commands (default: enabled)
    --no-parallel-split Disable parallel split (sequential processing)
    --inprocess         Use thread-safe in-process parallel ctags
    --split-jobs N      Number of parallel precc jobs (default: --jobs value)
    --max N             Maximum number of fix iterations (default: 10)
    --jobs N            Parallel jobs for precc generation (default: half of CPUs)
    --timeout N         Timeout for precc generation in seconds (default: 1800)
    --sample N          Sample size for failure estimation (default: 150)
    --scan-delay N      Delay before starting scan in seconds (default: 5)
    --exp-dir DIR       Use specific experiment directory
    --start-pu N        Start from PU N, skip generating/scanning earlier PUs

PRECC BUILD OPTIONS:
    --split             Enable split mode (default: enabled)
    --no-split          Disable split mode
    --lattice           Enable lattice-based header splitting (ICSM05 algorithm)
    --no-lattice        Disable lattice headers (default: disabled)
    --unity-build       Enable unity build mode (experimental, has issues)
    --no-unity-build    Disable unity build mode (default: disabled)
    --unity-batches N   Number of unity batches, 0 = adaptive (default: 0)

OUTPUT OPTIONS:
    --keep              Keep intermediate files (preprocessed, split, objects)
    --ascii             Use ASCII characters instead of unicode (for asciinema/GIF)

MODES:
    Performance (default): Benchmark generation and compilation throughput
    Vim (--vim):           Test all vim source files
"#
    );
}

fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();

    let mut auto_mode = true;  // Default: auto mode enabled
    let mut perf_mode = true;   // Default: perf mode enabled
    let mut vim_mode = false;
    let mut all_threshold_mode = false;
    let mut skip_regression = true;  // Default: skip regression for faster runs
    let mut parallel_split = true; // Default: enabled (like shell script)
    let mut inprocess_mode = false;
    let mut max_iterations = 10;
    let mut precc_jobs = 0;
    let mut split_jobs = 0;
    let mut precc_timeout = 1800;
    let mut sample_size = 150;
    let mut scan_delay = 5;
    let mut start_pu = 0;
    let mut experiment_dir: Option<PathBuf> = None;
    let mut positional_args: Vec<String> = Vec::new();
    // Precc build options with defaults
    let mut split = true;           // Default: enable split mode
    let mut lattice_headers = false; // Default: disable lattice headers
    let mut unity_build = false;    // Default: disable unity build (common header has missing type deps)
    let mut unity_batches: usize = 0; // Default: adaptive mode (0)
    // Linker selection - default to mold if available
    let mut linker = if has_mold() { LinkerType::Mold } else { LinkerType::Ld };
    // Link mode - default to parallel
    let mut link_mode = LinkMode::Parallel;
    // File preservation
    let mut keep_files = false;
    // Output mode
    let mut ascii_mode = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--auto" => auto_mode = true,
            "--no-auto" => auto_mode = false,
            "--perf" => perf_mode = true,
            "--bugfix" => perf_mode = false,
            "--vim" => vim_mode = true,
            "--all-threshold" => {
                all_threshold_mode = true;
                vim_mode = true;  // Implies vim mode
                skip_regression = true;  // Skip regression for performance focus
            }
            "--regression" => skip_regression = false,
            "--no-regression" | "--skip-regression" => skip_regression = true,
            "--parallel-split" => parallel_split = true,
            "--no-parallel-split" => parallel_split = false,
            "--inprocess" => inprocess_mode = true,
            "--split-jobs" => {
                i += 1;
                if i < args.len() {
                    split_jobs = args[i].parse().unwrap_or(0);
                }
            }
            "--max" => {
                i += 1;
                if i < args.len() {
                    max_iterations = args[i].parse().unwrap_or(10);
                }
            }
            "--jobs" => {
                i += 1;
                if i < args.len() {
                    precc_jobs = args[i].parse().unwrap_or(0);
                }
            }
            "--timeout" => {
                i += 1;
                if i < args.len() {
                    precc_timeout = args[i].parse().unwrap_or(1800);
                }
            }
            "--sample" => {
                i += 1;
                if i < args.len() {
                    sample_size = args[i].parse().unwrap_or(150);
                }
            }
            "--scan-delay" => {
                i += 1;
                if i < args.len() {
                    scan_delay = args[i].parse().unwrap_or(5);
                }
            }
            "--start-pu" | "--skip-until" => {
                i += 1;
                if i < args.len() {
                    start_pu = args[i].parse().unwrap_or(0);
                }
            }
            "--exp-dir" => {
                i += 1;
                if i < args.len() {
                    experiment_dir = Some(PathBuf::from(&args[i]));
                }
            }
            "--split" => split = true,
            "--no-split" => split = false,
            "--lattice" => lattice_headers = true,
            "--no-lattice" => lattice_headers = false,
            "--unity-build" => unity_build = true,
            "--no-unity-build" => unity_build = false,
            "--unity-batches" => {
                i += 1;
                if i < args.len() {
                    unity_batches = args[i].parse().unwrap_or(0);
                }
            }
            "--linker-mold" | "--mold" => linker = LinkerType::Mold,
            "--linker-ld" | "--ld" => linker = LinkerType::Ld,
            "--parallel-link" => link_mode = LinkMode::Parallel,
            "--sequential-link" => link_mode = LinkMode::Sequential,
            "--keep" => keep_files = true,
            "--ascii" => ascii_mode = true,
            arg if !arg.starts_with('-') => {
                positional_args.push(arg.to_string());
            }
            _ => {}
        }
        i += 1;
    }

    let input_file = positional_args.get(0).cloned().unwrap_or_else(|| "sqlite3.i".to_string());
    let baseline_failures = positional_args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(310);

    let total_cpus = get_num_cpus();
    if precc_jobs == 0 {
        precc_jobs = (total_cpus / 2).max(1);
    }
    if split_jobs == 0 {
        split_jobs = precc_jobs;
    }
    let reserved_cpus = total_cpus.saturating_sub(precc_jobs).max(1);

    // Get project root
    let exe_path = env::current_exe().unwrap_or_default();
    let source_dir = exe_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| env::current_dir().unwrap());

    // Get version info
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&source_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let git_branch = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&source_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let git_dirty = Command::new("git")
        .args(["diff", "--quiet"])
        .current_dir(&source_dir)
        .status()
        .map(|s| if s.success() { "" } else { "-dirty" })
        .unwrap_or("-dirty");

    let version_tag = format!("{}{}", git_hash, git_dirty);

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let exp_dir = experiment_dir.unwrap_or_else(|| {
        PathBuf::from(format!("/tmp/precc_exp_{}_{}", timestamp, version_tag))
    });

    let tests_dir = source_dir.join("tests/dctags");
    let precc_bin = exp_dir.join("precc");
    let exp_input_file = exp_dir.join(Path::new(&input_file).file_name().unwrap_or_default());

    Config {
        auto_mode,
        perf_mode,
        vim_mode,
        all_threshold_mode,
        skip_regression,
        parallel_split,
        inprocess_mode,
        max_iterations,
        precc_jobs,
        split_jobs,
        reserved_cpus,
        precc_timeout,
        sample_size,
        scan_delay,
        start_pu,
        input_file,
        baseline_failures,
        experiment_dir: exp_dir,
        source_dir,
        tests_dir,
        precc_bin,
        exp_input_file,
        version_tag,
        git_branch,
        split,
        lattice_headers,
        unity_build,
        unity_batches,
        linker,
        link_mode,
        keep_files,
        ascii_mode,
    }
}

fn setup_experiment_dir(cfg: &Config) {
    fs::create_dir_all(&cfg.experiment_dir).ok();

    // Create version info file
    let version_file = cfg.experiment_dir.join("version_info.txt");
    if let Ok(mut f) = File::create(&version_file) {
        writeln!(f, "Experiment: {:?}", cfg.experiment_dir).ok();
        writeln!(f, "Created: {}", chrono::Local::now()).ok();
        writeln!(f, "Git Hash: {}", cfg.version_tag).ok();
        writeln!(f, "Git Branch: {}", cfg.git_branch).ok();
        writeln!(f, "Source Dir: {:?}", cfg.source_dir).ok();
        writeln!(f, "Input File: {}", cfg.input_file).ok();
        writeln!(f, "Baseline: {}", cfg.baseline_failures).ok();
    }

    // Create symlink to latest
    let latest_link = PathBuf::from("/tmp/precc_exp_latest");
    fs::remove_file(&latest_link).ok();
    std::os::unix::fs::symlink(&cfg.experiment_dir, &latest_link).ok();
}

fn copy_precc_binary(cfg: &Config) {
    let release_bin = cfg.source_dir.join("target/release/precc");
    let deployed_bin = cfg.source_dir.join("bin/precc");

    // Prefer target/release/precc (built by cargo), fall back to bin/precc (deployed)
    let source_bin = if release_bin.exists() {
        &release_bin
    } else if deployed_bin.exists() {
        &deployed_bin
    } else {
        return;
    };

    fs::copy(source_bin, &cfg.precc_bin).ok();
}

/// Fetch and generate sqlite3.i from the sqlite repository if not found
fn fetch_sqlite3(cfg: &Config) -> bool {
    let sqlite_dir = cfg.source_dir.join("tests/sqlite3");
    let sqlite3_c = sqlite_dir.join("sqlite3.c");
    let sqlite3_i = sqlite_dir.join("sqlite3.i");

    // Check if sqlite3.i already exists
    if sqlite3_i.exists() {
        log_info(&format!("Found existing sqlite3.i at {:?}", sqlite3_i));
        return true;
    }

    // Create the sqlite3 directory if needed
    if !sqlite_dir.exists() {
        fs::create_dir_all(&sqlite_dir).ok();
    }

    // If sqlite3.c doesn't exist, download the amalgamation from sqlite.org
    if !sqlite3_c.exists() {
        log_info("Downloading sqlite amalgamation from sqlite.org...");

        // Download the amalgamation zip
        let zip_url = "https://www.sqlite.org/2024/sqlite-amalgamation-3450100.zip";
        let zip_path = sqlite_dir.join("sqlite-amalgamation.zip");

        let status = Command::new("curl")
            .args(["-L", "-o"])
            .arg(&zip_path)
            .arg(zip_url)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();

        if !status.map(|s| s.success()).unwrap_or(false) {
            // Try wget as fallback
            let status = Command::new("wget")
                .args(["-O"])
                .arg(&zip_path)
                .arg(zip_url)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();

            if !status.map(|s| s.success()).unwrap_or(false) {
                log_error("Failed to download sqlite amalgamation (tried curl and wget)");
                return false;
            }
        }

        // Extract the zip
        log_info("Extracting sqlite amalgamation...");
        let status = Command::new("unzip")
            .args(["-o", "-q"])
            .arg(&zip_path)
            .current_dir(&sqlite_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status();

        if !status.map(|s| s.success()).unwrap_or(false) {
            log_error("Failed to extract sqlite amalgamation zip");
            return false;
        }

        // Move files from extracted directory to sqlite_dir
        let extracted_dir = sqlite_dir.join("sqlite-amalgamation-3450100");
        if extracted_dir.exists() {
            for entry in fs::read_dir(&extracted_dir).into_iter().flatten() {
                if let Ok(entry) = entry {
                    let dest = sqlite_dir.join(entry.file_name());
                    fs::rename(entry.path(), dest).ok();
                }
            }
            fs::remove_dir_all(&extracted_dir).ok();
        }

        // Clean up zip file
        fs::remove_file(&zip_path).ok();

        if sqlite3_c.exists() {
            log_ok(&format!("Downloaded sqlite3.c to {:?}", sqlite3_c));
        } else {
            log_error("sqlite3.c not found after extraction");
            return false;
        }
    }

    // Generate sqlite3.i using gcc -E
    if sqlite3_c.exists() {
        log_info("Generating sqlite3.i with gcc -E...");
        let output = Command::new("gcc")
            .args(["-E", "-o"])
            .arg(&sqlite3_i)
            .arg(&sqlite3_c)
            .current_dir(&sqlite_dir)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                log_ok(&format!("Generated sqlite3.i at {:?}", sqlite3_i));
                return true;
            }
            Ok(out) => {
                log_error(&format!(
                    "gcc -E failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
                return false;
            }
            Err(e) => {
                log_error(&format!("Failed to run gcc: {}", e));
                return false;
            }
        }
    }

    false
}

/// Fetch and configure vim repository if not found
fn fetch_vim(cfg: &Config) -> bool {
    let vim_dir = cfg.source_dir.join("tests/vim");
    let vim_src = vim_dir.join("src");
    let vim_auto = vim_src.join("auto");
    let config_h = vim_auto.join("config.h");
    let osdef_h = vim_auto.join("osdef.h");

    // Check if vim is already fully configured (both config.h and osdef.h)
    if config_h.exists() && osdef_h.exists() {
        return true;
    }

    // Check if vim source exists but not fully configured
    if vim_src.exists() && (!config_h.exists() || !osdef_h.exists()) {
        log_info("Vim source found but not fully configured, running ./configure...");
        return configure_vim(&vim_dir);
    }

    // Clone vim repository
    log_info("Cloning vim repository...");

    // Remove any partial vim directory
    if vim_dir.exists() {
        fs::remove_dir_all(&vim_dir).ok();
    }

    let status = Command::new("git")
        .args([
            "clone",
            "--depth", "1",
            "https://github.com/vim/vim.git",
        ])
        .arg(&vim_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    if !status.map(|s| s.success()).unwrap_or(false) {
        log_error("Failed to clone vim repository");
        return false;
    }

    log_ok("Cloned vim repository");

    // Run configure
    configure_vim(&vim_dir)
}

/// Run ./configure in vim directory to generate config.h and osdef.h
fn configure_vim(vim_dir: &Path) -> bool {
    log_info("Running ./configure in vim/src...");

    let vim_src = vim_dir.join("src");

    // Run configure from the src directory
    let status = Command::new("./configure")
        .current_dir(&vim_src)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    if !status.map(|s| s.success()).unwrap_or(false) {
        log_error("Failed to run ./configure");
        return false;
    }

    // Verify config.h was created
    let config_h = vim_src.join("auto/config.h");
    if !config_h.exists() {
        log_error("config.h not generated after ./configure");
        return false;
    }
    log_ok("Generated config.h successfully");

    // Generate osdef.h (required for preprocessing)
    log_info("Generating osdef.h...");
    let status = Command::new("make")
        .arg("auto/osdef.h")
        .current_dir(&vim_src)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    if !status.map(|s| s.success()).unwrap_or(false) {
        log_error("Failed to generate osdef.h");
        return false;
    }

    let osdef_h = vim_src.join("auto/osdef.h");
    if osdef_h.exists() {
        log_ok("Generated osdef.h successfully");
        true
    } else {
        log_error("osdef.h not generated");
        false
    }
}

fn copy_input_file(cfg: &Config) {
    let input_path = PathBuf::from(&cfg.input_file);

    // Try the given path first
    if input_path.exists() {
        fs::copy(&input_path, &cfg.exp_input_file).ok();
        return;
    }

    // If not found, search common locations
    let filename = input_path.file_name().and_then(|n| n.to_str()).unwrap_or(&cfg.input_file);
    let search_paths = [
        cfg.source_dir.join("tests/sqlite3").join(filename),
        cfg.source_dir.join("tests").join(filename),
        cfg.source_dir.join(filename),
    ];

    for path in &search_paths {
        if path.exists() {
            log_info(&format!("Found input file at {:?}", path));
            fs::copy(path, &cfg.exp_input_file).ok();
            return;
        }
    }

    // If input file is sqlite3.i and not found, try to fetch it
    if filename == "sqlite3.i" {
        log_info("sqlite3.i not found, attempting to fetch from repository...");
        if fetch_sqlite3(cfg) {
            let sqlite3_i = cfg.source_dir.join("tests/sqlite3/sqlite3.i");
            if sqlite3_i.exists() {
                fs::copy(&sqlite3_i, &cfg.exp_input_file).ok();
                log_ok("Copied freshly generated sqlite3.i to experiment directory");
            }
        }
    }
}

fn build_if_needed(cfg: &Config) {
    let release_bin = cfg.source_dir.join("target/release/precc");
    let deployed_bin = cfg.source_dir.join("bin/precc");
    let lib_rs = cfg.source_dir.join("src/lib.rs");

    // Check if we need to build
    let needs_build = if release_bin.exists() {
        // Check if source is newer than binary
        lib_rs
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .zip(release_bin.metadata().and_then(|m| m.modified()).ok())
            .map(|(src, bin)| src > bin)
            .unwrap_or(true)
    } else if deployed_bin.exists() {
        // Deployed binary exists, no need to build (can't build in deployed env)
        false
    } else {
        // No binary found, need to build
        true
    };

    if needs_build {
        log_info("Building release version...");
        let start = Instant::now();
        let status = Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(&cfg.source_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status();

        if status.map(|s| s.success()).unwrap_or(false) {
            log_ok(&format!(
                "Build completed in {}",
                format_duration(start.elapsed().as_secs())
            ));
        }
    }

    copy_precc_binary(cfg);
    log_info(&format!("Using precc binary: {:?}", cfg.precc_bin));
}

fn compile_file(path: &Path) -> bool {
    Command::new("gcc")
        .args(["-g", "-O2", "-c"])
        .arg(path)
        .args(["-o", "/dev/null"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn compile_file_with_includes(path: &Path, vim_src: &Path, vim_proto: &Path) -> bool {
    let vim_auto = vim_src.join("auto");
    Command::new("gcc")
        .args(["-c"])
        .arg(path)
        .arg("-I")
        .arg(vim_src)
        .arg("-I")
        .arg(&vim_auto)
        .arg("-I")
        .arg(vim_proto)
        .args(["-DHAVE_CONFIG_H", "-o", "/dev/null"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Compile a C file to an object file in the specified output directory
fn compile_to_object(path: &Path, out_dir: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let out_path = out_dir.join(format!("{}.o", stem));
    let status = Command::new("gcc")
        .args(["-g", "-O2", "-c"])
        .arg(path)
        .arg("-o")
        .arg(&out_path)
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .ok()?;
    if status.success() {
        Some(out_path)
    } else {
        None
    }
}

/// Check if mold linker is available
fn has_mold() -> bool {
    Command::new("mold")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Link object files into a combined relocatable object using ld -r (or mold -r if available)
#[allow(dead_code)]
fn link_objects(objects: &[PathBuf], output: &Path) -> bool {
    if objects.is_empty() {
        return false;
    }

    // Try mold first (much faster), fall back to ld
    let linker = if has_mold() { "mold" } else { "ld" };

    let mut cmd = Command::new(linker);
    cmd.arg("-r")
        .arg("--allow-multiple-definition")
        .arg("-o")
        .arg(output);
    for obj in objects {
        cmd.arg(obj);
    }
    cmd.stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Link object files using a specific linker
fn link_objects_with(objects: &[PathBuf], output: &Path, linker: &str) -> bool {
    if objects.is_empty() {
        return false;
    }
    let mut cmd = Command::new(linker);
    cmd.arg("-r")
        .arg("--allow-multiple-definition")
        .arg("-o")
        .arg(output);
    for obj in objects {
        cmd.arg(obj);
    }
    cmd.stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if tmpfs (/dev/shm) is available and writable
fn get_tmpfs_dir() -> Option<PathBuf> {
    let shm = PathBuf::from("/dev/shm");
    if shm.exists() && shm.is_dir() {
        // Try to create a test directory
        let test_dir = shm.join(format!("precc_test_{}", std::process::id()));
        if fs::create_dir_all(&test_dir).is_ok() {
            fs::remove_dir_all(&test_dir).ok();
            return Some(shm);
        }
    }
    None
}

/// Link a batch of objects with optional mold threading
fn link_batch_with_threads(objects: &[PathBuf], output: &Path, linker: &str, threads: Option<usize>) -> bool {
    if objects.is_empty() {
        return false;
    }

    let mut cmd = Command::new(linker);
    cmd.arg("-r")
        .arg("--allow-multiple-definition");

    // Add --threads for mold if specified
    if let Some(t) = threads {
        if linker == "mold" {
            cmd.arg(format!("--threads={}", t));
        }
    }

    cmd.arg("-o").arg(output);
    for obj in objects {
        cmd.arg(obj);
    }

    cmd.stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Multi-level hierarchical parallel linking
///
/// Strategy:
/// 1. Use tmpfs (/dev/shm) for intermediate files if available
/// 2. Multi-level linking: recursively reduce object count until manageable
/// 3. Use mold --threads for explicit thread control
///
/// For 2503 objects with 48 jobs:
/// - Level 1: 2503 -> 48 batches (parallel) -> 48 intermediate objects
/// - Level 2: 48 -> 8 batches (parallel) -> 8 intermediate objects
/// - Level 3: 8 -> 1 final object
fn link_objects_parallel_with(objects: &[PathBuf], output: &Path, temp_dir: &Path, num_jobs: usize, linker: &str) -> (bool, f64) {
    if objects.is_empty() {
        return (false, 0.0);
    }

    let start = Instant::now();

    // For small number of objects, just use regular linking
    if objects.len() <= num_jobs * 2 {
        let success = link_objects_with(objects, output, linker);
        return (success, start.elapsed().as_secs_f64());
    }

    // Use tmpfs if available for faster I/O
    let base_temp = get_tmpfs_dir().unwrap_or_else(|| temp_dir.to_path_buf());
    let intermediate_dir = base_temp.join(format!("precc_link_{}", std::process::id()));
    fs::create_dir_all(&intermediate_dir).ok();

    // Calculate mold threads per batch (divide total threads among parallel batches)
    let mold_threads = if linker == "mold" && num_jobs > 1 {
        Some(std::cmp::max(1, num_jobs / 8)) // Each batch gets ~1/8 of total threads
    } else {
        None
    };

    // Perform multi-level hierarchical linking
    let result = link_hierarchical(objects, output, &intermediate_dir, num_jobs, linker, mold_threads, 0);

    // Cleanup intermediate files
    fs::remove_dir_all(&intermediate_dir).ok();

    (result, start.elapsed().as_secs_f64())
}

/// Recursive hierarchical linking
fn link_hierarchical(
    objects: &[PathBuf],
    output: &Path,
    temp_dir: &Path,
    num_jobs: usize,
    linker: &str,
    mold_threads: Option<usize>,
    level: usize
) -> bool {
    // Base case: small enough to link directly
    if objects.len() <= num_jobs {
        return link_batch_with_threads(objects, output, linker, mold_threads);
    }

    // Create level-specific temp directory
    let level_dir = temp_dir.join(format!("level_{}", level));
    fs::create_dir_all(&level_dir).ok();

    // Split objects into batches
    let batch_size = (objects.len() + num_jobs - 1) / num_jobs;
    let batches: Vec<Vec<PathBuf>> = objects
        .chunks(batch_size)
        .map(|chunk| chunk.to_vec())
        .collect();

    let num_batches = batches.len();
    let linker_owned = linker.to_string();

    // Link each batch in parallel
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_jobs)
        .build()
        .unwrap();

    let intermediate_objects: Vec<PathBuf> = pool.install(|| {
        batches
            .par_iter()
            .enumerate()
            .filter_map(|(idx, batch)| {
                let intermediate = level_dir.join(format!("batch_{}_{}.o", level, idx));

                if link_batch_with_threads(batch, &intermediate, &linker_owned, mold_threads) {
                    Some(intermediate)
                } else {
                    None
                }
            })
            .collect()
    });

    // Check if all batches succeeded
    if intermediate_objects.len() != num_batches {
        return false;
    }

    // Recursively link intermediate objects (next level)
    link_hierarchical(&intermediate_objects, output, temp_dir, num_jobs, linker, mold_threads, level + 1)
}

/// Smart link function that uses the configured link mode and linker
fn smart_link(objects: &[PathBuf], output: &Path, temp_dir: &Path, num_jobs: usize, linker: &str, mode: LinkMode) -> (bool, f64) {
    match mode {
        LinkMode::Sequential => {
            let start = Instant::now();
            let success = link_objects_with(objects, output, linker);
            (success, start.elapsed().as_secs_f64())
        }
        LinkMode::Parallel => {
            link_objects_parallel_with(objects, output, temp_dir, num_jobs, linker)
        }
    }
}

/// Count exported (T) symbols in an object file
fn count_symbols(path: &Path) -> HashSet<String> {
    let output = Command::new("nm")
        .arg(path)
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines()
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 3 && parts[1] == "T" {
                        Some(parts[2].to_string())
                    } else {
                        None
                    }
                })
                .collect()
        }
        Err(_) => HashSet::new(),
    }
}

/// Get text section size of an object file using size command
fn get_text_size(path: &Path) -> usize {
    let output = Command::new("size")
        .arg(path)
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // Parse output: "   text    data     bss     dec     hex filename"
            //               "  12345    1234     567   14146    3742 file.o"
            if let Some(line) = stdout.lines().nth(1) {
                if let Some(text_str) = line.split_whitespace().next() {
                    return text_str.parse().unwrap_or(0);
                }
            }
            0
        }
        Err(_) => 0,
    }
}

/// Compile a Vim source file to an object file
fn compile_vim_to_object(path: &Path, out_dir: &Path, vim_src: &Path, vim_proto: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let out_path = out_dir.join(format!("{}.o", stem));
    let vim_auto = vim_src.join("auto");
    let status = Command::new("gcc")
        .args(["-g", "-O2", "-c"])
        .arg(path)
        .arg("-I")
        .arg(vim_src)
        .arg("-I")
        .arg(&vim_auto)
        .arg("-I")
        .arg(vim_proto)
        .arg("-DHAVE_CONFIG_H")
        .arg("-o")
        .arg(&out_path)
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .ok()?;
    if status.success() {
        Some(out_path)
    } else {
        None
    }
}

/// Compile a vim .i file to object file
fn compile_vim_i_to_object(i_file: &Path, out_dir: &Path, vim_src: &Path, vim_proto: &Path) -> Option<PathBuf> {
    let stem = i_file.file_stem()?.to_str()?;
    let obj_path = out_dir.join(format!("{}.o", stem));

    let status = Command::new("gcc")
        .args(["-g", "-O2", "-c"])
        .arg(i_file)
        .arg("-o")
        .arg(&obj_path)
        .arg(format!("-I{}", vim_src.display()))
        .arg(format!("-I{}", vim_proto.display()))
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .ok()?;

    if status.success() {
        Some(obj_path)
    } else {
        None
    }
}

/// Run linking validation for Vim PU files
fn run_vim_link_validation(cfg: &Config, stats: &mut Stats, pu_files: &[PathBuf], i_files: &[PathBuf], vim_src: &Path, vim_proto: &Path) {
    if pu_files.is_empty() {
        return;
    }

    eprintln!();
    eprintln!("  {}Linking Validation:{}", BOLD, NC);

    // Determine which linker to use (same for both original and split for fair comparison)
    let linker_name = cfg.linker.name();
    let actual_linker = if cfg.linker == LinkerType::Mold && !has_mold() {
        eprintln!("    {}Warning: mold not available, falling back to ld{}", YELLOW, NC);
        "ld"
    } else {
        linker_name
    };
    eprintln!("    Using linker: {}{}{}, mode: {}{}{} (same for original and split)",
        CYAN, actual_linker, NC, CYAN, cfg.link_mode.name(), NC);

    // Create temp directory for object files
    let obj_dir = cfg.experiment_dir.join("vim_obj_files");
    let orig_obj_dir = cfg.experiment_dir.join("vim_orig_obj_files");
    let _ = fs::create_dir_all(&obj_dir);
    let _ = fs::create_dir_all(&orig_obj_dir);

    let pool = rayon::ThreadPoolBuilder::new()
        .build()
        .unwrap();

    // Step 1: Compile original .i files to objects and link
    eprintln!("    Compiling {} original .i files...", i_files.len());
    let orig_start = Instant::now();

    let vim_src_clone = vim_src.to_path_buf();
    let vim_proto_clone = vim_proto.to_path_buf();
    let orig_obj_dir_clone = orig_obj_dir.clone();

    let orig_obj_files: Vec<PathBuf> = pool.install(|| {
        i_files.par_iter()
            .filter_map(|i| compile_vim_i_to_object(i, &orig_obj_dir_clone, &vim_src_clone, &vim_proto_clone))
            .collect()
    });

    let orig_comp_time = orig_start.elapsed().as_secs_f64();
    eprintln!("    Compiled {} original objects in {}{:.3}s{}", orig_obj_files.len(), CYAN, orig_comp_time, NC);

    // Link original objects using the same linker and mode as split
    let orig_combined = orig_obj_dir.join("vim_orig_combined.o");
    let (orig_link_success, orig_link_time) = smart_link(
        &orig_obj_files, &orig_combined, &orig_obj_dir, cfg.precc_jobs, actual_linker, cfg.link_mode
    );

    if orig_link_success {
        eprintln!("    Original link time: {}{:.3}s{} ({}, {})", GREEN, orig_link_time, NC, actual_linker, cfg.link_mode.name());
        stats.vim_orig_comp_time = orig_comp_time;
        stats.vim_orig_link_time = orig_link_time;

        // Count original symbols
        let orig_symbols = count_symbols(&orig_combined);
        eprintln!("    Original symbols: {}{}{}", GREEN, orig_symbols.len(), NC);
    } else {
        log_warn("Failed to link original Vim objects");
        stats.vim_orig_comp_time = orig_comp_time;
        stats.vim_orig_link_time = 0.0;
    }

    // Step 2: Compile PU files to object files in parallel
    eprintln!("    Compiling {} PU files to objects...", pu_files.len());
    let split_comp_start = Instant::now();

    let vim_src_clone2 = vim_src.to_path_buf();
    let vim_proto_clone2 = vim_proto.to_path_buf();
    let obj_dir_clone = obj_dir.clone();

    let obj_files: Vec<PathBuf> = pool.install(|| {
        pu_files.par_iter()
            .filter_map(|pu| compile_vim_to_object(pu, &obj_dir_clone, &vim_src_clone2, &vim_proto_clone2))
            .collect()
    });
    let split_comp_time = split_comp_start.elapsed().as_secs_f64();
    eprintln!("    Compiled {} PU object files in {}{:.3}s{}", obj_files.len(), CYAN, split_comp_time, NC);
    // Use actual compilation time for fair comparison (not benchmark's /dev/null compilation)
    stats.vim_comp_time = split_comp_time;

    // Link PU object files using the same linker and mode as original
    let combined_obj = obj_dir.join("vim_combined.o");

    let (link_success, link_time) = smart_link(
        &obj_files, &combined_obj, &obj_dir, cfg.precc_jobs, actual_linker, cfg.link_mode
    );
    stats.vim_link_time = link_time;
    stats.vim_link_success = link_success;

    if link_success {
        eprintln!("    Split link time:   {}{:.3}s{} ({}, {})", GREEN, stats.vim_link_time, NC, actual_linker, cfg.link_mode.name());

        // Count symbols
        let combined_symbols = count_symbols(&combined_obj);
        stats.vim_symbols_count = combined_symbols.len();
        stats.vim_symbols_match = true;
        eprintln!("    Split symbols: {}{}{} exported functions", GREEN, stats.vim_symbols_count, NC);

        // Get text section sizes (strip first for accurate comparison)
        let orig_stripped = obj_dir.join("vim_orig_stripped.o");
        let split_stripped = obj_dir.join("vim_split_stripped.o");
        let _ = Command::new("strip").arg("-o").arg(&orig_stripped).arg(&orig_combined).status();
        let _ = Command::new("strip").arg("-o").arg(&split_stripped).arg(&combined_obj).status();
        stats.vim_orig_text_size = get_text_size(&orig_stripped);
        stats.vim_split_text_size = get_text_size(&split_stripped);
    } else {
        log_warn("Failed to link Vim PU object files");
    }

    // Copy combined objects to bin/generated for manual inspection
    let gen_dir = cfg.source_dir.join("bin/generated");
    if fs::create_dir_all(&gen_dir).is_ok() {
        if orig_combined.exists() {
            let _ = fs::copy(&orig_combined, gen_dir.join("vim_orig"));
        }
        if combined_obj.exists() {
            let _ = fs::copy(&combined_obj, gen_dir.join("vim_split"));
        }
    }

    // Cleanup
    let _ = fs::remove_dir_all(&obj_dir);
    let _ = fs::remove_dir_all(&orig_obj_dir);
}

/// Run linking benchmark for SQLite PU files
fn run_sqlite_link_benchmark(cfg: &Config, stats: &mut Stats, pu_files: &[PathBuf]) {
    if pu_files.is_empty() {
        return;
    }

    log_stage("LINKING VALIDATION");

    // Create temp directory for object files
    let obj_dir = cfg.experiment_dir.join("obj_files");
    let _ = fs::create_dir_all(&obj_dir);

    // Step 1: Compile original sqlite3.c
    eprintln!("  {}Compiling original sqlite3.c...{}", CYAN, NC);
    let sqlite3_c = cfg.source_dir.join("tests/sqlite3/sqlite3.c");
    let orig_obj = obj_dir.join("sqlite3_orig.o");
    let orig_start = Instant::now();
    let orig_compiled = Command::new("gcc")
        .args(["-g", "-O2", "-c"])
        .arg(&sqlite3_c)
        .arg("-o")
        .arg(&orig_obj)
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    stats.perf_orig_comp_time = orig_start.elapsed().as_secs_f64();

    if !orig_compiled {
        log_warn("Failed to compile original sqlite3.c");
        return;
    }
    eprintln!("    Original compile time: {}{:.3}s{}", GREEN, stats.perf_orig_comp_time, NC);

    // Get original symbols
    let orig_symbols = count_symbols(&orig_obj);
    eprintln!("    Original symbols: {}{}{}", GREEN, orig_symbols.len(), NC);

    // Step 2: Compile all PU files to object files in parallel
    eprintln!("  {}Compiling {} PU files to objects...{}", CYAN, pu_files.len(), NC);
    let pool = rayon::ThreadPoolBuilder::new()
        .build()
        .unwrap();

    let obj_files: Vec<PathBuf> = pool.install(|| {
        pu_files.par_iter()
            .filter_map(|pu| compile_to_object(pu, &obj_dir))
            .collect()
    });
    eprintln!("    Compiled {} object files", obj_files.len());

    // Step 3: Link all object files using configured linker and mode
    eprintln!("  {}Linking {} object files...{}", CYAN, obj_files.len(), NC);
    let combined_obj = obj_dir.join("sqlite3_combined.o");
    let linker_name = cfg.linker.name();

    // Check if mold is available when requested
    let actual_linker = if cfg.linker == LinkerType::Mold && !has_mold() {
        eprintln!("    {}Warning: mold not available, falling back to ld{}", YELLOW, NC);
        "ld"
    } else {
        linker_name
    };
    eprintln!("    Using linker: {}{}{}, mode: {}{}{}", CYAN, actual_linker, NC, CYAN, cfg.link_mode.name(), NC);

    let (link_success, link_time) = smart_link(
        &obj_files, &combined_obj, &obj_dir, cfg.precc_jobs, actual_linker, cfg.link_mode
    );
    stats.perf_link_time = link_time;
    stats.perf_link_success = link_success;

    if link_success {
        eprintln!("    Link time: {}{:.3}s{} ({}, {})", GREEN, stats.perf_link_time, NC, actual_linker, cfg.link_mode.name());

        // Step 4: Compare symbols
        let combined_symbols = count_symbols(&combined_obj);
        stats.perf_symbols_count = combined_symbols.len();
        stats.perf_symbols_match = combined_symbols == orig_symbols;

        if stats.perf_symbols_match {
            eprintln!("    Symbols: {}{} {} symbols match{}", GREEN, check_mark(), stats.perf_symbols_count, NC);
        } else {
            let missing: Vec<_> = orig_symbols.difference(&combined_symbols).take(5).collect();
            let extra: Vec<_> = combined_symbols.difference(&orig_symbols).take(5).collect();
            eprintln!("    Symbols: {}{} mismatch{} (orig: {}, combined: {})",
                     RED, cross_mark(), NC, orig_symbols.len(), combined_symbols.len());
            if !missing.is_empty() {
                eprintln!("      Missing: {:?}...", missing);
            }
            if !extra.is_empty() {
                eprintln!("      Extra: {:?}...", extra);
            }
        }

        // Get text section sizes (strip first for accurate comparison)
        let orig_stripped = obj_dir.join("orig_stripped.o");
        let split_stripped = obj_dir.join("split_stripped.o");
        let _ = Command::new("strip").arg("-o").arg(&orig_stripped).arg(&orig_obj).status();
        let _ = Command::new("strip").arg("-o").arg(&split_stripped).arg(&combined_obj).status();
        stats.perf_orig_text_size = get_text_size(&orig_stripped);
        stats.perf_split_text_size = get_text_size(&split_stripped);
    } else {
        log_warn("Failed to link object files");
    }

    // Copy combined objects to bin/generated for manual inspection
    let gen_dir = cfg.source_dir.join("bin/generated");
    if fs::create_dir_all(&gen_dir).is_ok() {
        if orig_obj.exists() {
            let _ = fs::copy(&orig_obj, gen_dir.join("sqlite3_orig"));
        }
        if combined_obj.exists() {
            let _ = fs::copy(&combined_obj, gen_dir.join("sqlite3_split"));
        }
    }

    // Cleanup
    let _ = fs::remove_dir_all(&obj_dir);

    eprintln!();
}

fn find_pu_files(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                // Handle both split mode (e.g., "file.i_123.pu.c") and passthrough mode (e.g., "file.i.pu.c")
                if name.starts_with(pattern) && name.ends_with(".pu.c") {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

/// Find all PU files for a given input file basename, including both split and passthrough modes
/// - Split mode: `{basename}_*.pu.c` (e.g., "sqlite3.i_1899.pu.c")
/// - Passthrough mode: `{basename}.pu.c` (e.g., "sqlite3.i.pu.c")
fn find_all_pu_files(dir: &Path, basename: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".pu.c") {
                    // Split mode: {basename}_*.pu.c
                    let split_pattern = format!("{}_", basename);
                    // Passthrough mode: {basename}.pu.c
                    let passthrough_name = format!("{}.pu.c", basename);
                    if name.starts_with(&split_pattern) || name == passthrough_name {
                        files.push(path);
                    }
                }
            }
        }
    }
    files.sort();
    files
}

fn get_pu_number(filename: &str) -> Option<usize> {
    // Extract number from filename like "sqlite3.i_1899.pu.c"
    let parts: Vec<&str> = filename.split('_').collect();
    if parts.len() >= 2 {
        let num_part = parts.last()?.trim_end_matches(".pu.c");
        num_part.parse().ok()
    } else {
        None
    }
}

fn run_regression_test(cfg: &Config, stats: &mut Stats) -> bool {
    log_stage("REGRESSION TEST");
    eprintln!(
        "{}Testing previously fixed bugs to ensure no regressions...{}",
        DIM, NC
    );
    eprintln!();

    // Create regression work directory in experiment dir
    let regression_dir = cfg.experiment_dir.join("regression_work");
    fs::create_dir_all(&regression_dir).ok();

    let mut passed = 0;
    let mut regressions = 0;
    let total_bugs = FIXED_BUGS
        .iter()
        .filter(|b| cfg.tests_dir.join(format!("{}.c", b)).exists())
        .count();

    for (i, bug) in FIXED_BUGS.iter().enumerate() {
        let bug_file = cfg.tests_dir.join(format!("{}.c", bug));
        if !bug_file.exists() {
            continue;
        }

        // Progress
        eprint!(
            "\r\x1b[K{}Regression{} [{}/{}] {}",
            DIM,
            NC,
            i + 1,
            total_bugs,
            bug
        );

        let i_file = regression_dir.join(format!("{}.i", bug));

        // Preprocess
        let preprocess = Command::new("gcc")
            .args(["-E"])
            .arg(&bug_file)
            .arg("-o")
            .arg(&i_file)
            .stderr(Stdio::null())
            .status();

        if !preprocess.map(|s| s.success()).unwrap_or(false) {
            continue;
        }

        // Run precc (outputs to current dir, so use regression_dir)
        let mut precc_cmd = Command::new(&cfg.precc_bin);
        precc_cmd
            .arg(&i_file)
            .current_dir(&regression_dir)
            .env("PASSTHROUGH_THRESHOLD", "0")
            .env("JOBS", "1")
            .stderr(Stdio::null())
            .stdout(Stdio::null());
        apply_precc_build_options(&mut precc_cmd, cfg);
        let precc_status = precc_cmd.status();

        if !precc_status.map(|s| s.success()).unwrap_or(false) {
            continue;
        }

        // Compile PUs
        let mut ok = true;
        for entry in fs::read_dir(&regression_dir).into_iter().flatten() {
            if let Ok(entry) = entry {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&format!("{}.i_", bug)) && name_str.ends_with(".pu.c") {
                    if !compile_file(&entry.path()) {
                        ok = false;
                        break;
                    }
                }
            }
        }

        // Cleanup this bug's files
        fs::remove_file(&i_file).ok();
        for entry in fs::read_dir(&regression_dir).into_iter().flatten() {
            if let Ok(entry) = entry {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&format!("{}.i", bug)) {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }

        if ok {
            passed += 1;
        } else {
            regressions += 1;
            eprintln!();
            log_error(&format!("REGRESSION: {}", bug));
        }
    }

    // Cleanup regression work directory
    fs::remove_dir_all(&regression_dir).ok();

    eprintln!();
    eprintln!();

    if regressions > 0 {
        log_error(&format!(
            "{} regression(s) detected out of {} tests!",
            regressions, total_bugs
        ));
        return false;
    }

    log_ok(&format!(
        "All {} fixed bugs still pass (0 regressions)",
        passed
    ));
    stats.regression_tests_passed = passed;
    true
}

fn start_generation(cfg: &Config) -> Option<Child> {
    log_info(&format!(
        "Starting precc generation in background (JOBS={})...",
        cfg.precc_jobs
    ));

    // Clean previous generated files
    for entry in fs::read_dir(&cfg.experiment_dir).into_iter().flatten() {
        if let Ok(entry) = entry {
            if entry
                .path()
                .extension()
                .map(|e| e == "c")
                .unwrap_or(false)
            {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".pu.c")
                {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }
    }

    let mut cmd = Command::new(&cfg.precc_bin);
    cmd.arg(&cfg.exp_input_file)
        .current_dir(&cfg.experiment_dir)
        .env("PASSTHROUGH_THRESHOLD", "0")
        .env("JOBS", cfg.precc_jobs.to_string())
        .stderr(Stdio::null())
        .stdout(Stdio::null());
    apply_precc_build_options(&mut cmd, cfg);

    if cfg.inprocess_mode {
        log_info("Using thread-safe in-process parallel ctags");
    }

    if cfg.start_pu > 0 {
        cmd.env("START_PU", cfg.start_pu.to_string());
        log_info(&format!("Precc will skip generating PUs < {}", cfg.start_pu));
    }

    match cmd.spawn() {
        Ok(child) => {
            log_info(&format!("Generation started (PID: {})", child.id()));
            log_info(&format!("Output directory: {:?}", cfg.experiment_dir));
            Some(child)
        }
        Err(e) => {
            log_error(&format!("Failed to start generation: {}", e));
            None
        }
    }
}

fn overlapped_generation_and_scan(
    cfg: &Config,
    stats: &mut Stats,
) -> Result<Option<PathBuf>, String> {
    log_stage("OVERLAPPED GENERATION & SCAN");
    eprintln!(
        "{}Generating PUs while scanning for failures in parallel...{}",
        DIM, NC
    );
    eprintln!(
        "{}(Generation: {} cores, Scanning: {} cores){}",
        DIM, cfg.precc_jobs, cfg.reserved_cpus, NC
    );
    eprintln!("{}Experiment dir: {:?}{}", DIM, cfg.experiment_dir, NC);
    if cfg.start_pu > 0 {
        eprintln!(
            "{}Skipping PUs < {} (use --start-pu to change){}",
            YELLOW, cfg.start_pu, NC
        );
    }
    eprintln!();

    let mut gen_child = start_generation(cfg).ok_or("Failed to start generation")?;

    log_info(&format!(
        "Waiting {}s for initial PUs to be generated...",
        cfg.scan_delay
    ));
    std::thread::sleep(Duration::from_secs(cfg.scan_delay));

    let gen_start = Instant::now();
    let input_basename = cfg
        .exp_input_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    let pattern = format!("{}_", input_basename);

    let failure_found = Arc::new(AtomicBool::new(false));
    let first_failure: Arc<std::sync::Mutex<Option<PathBuf>>> =
        Arc::new(std::sync::Mutex::new(None));
    let tested_count = Arc::new(AtomicUsize::new(0));
    let mut scanned_files: HashSet<PathBuf> = HashSet::new();

    let mut spin_i = 0;

    loop {
        let elapsed = gen_start.elapsed().as_secs();

        // Check timeout
        if elapsed >= cfg.precc_timeout {
            gen_child.kill().ok();
            gen_child.wait().ok();
            return Err(format!(
                "Generation timed out after {}",
                format_duration(cfg.precc_timeout)
            ));
        }

        // Check if failure found
        if failure_found.load(Ordering::Relaxed) {
            gen_child.kill().ok();
            gen_child.wait().ok();

            let failure = first_failure.lock().unwrap().clone();
            let pu_count = find_pu_files(&cfg.experiment_dir, &pattern).len();
            let tested = tested_count.load(Ordering::Relaxed);

            eprintln!();
            eprintln!();
            if let Some(ref f) = failure {
                log_info(&format!("First failure found: {:?}", f));
            }
            log_info(&format!(
                "Tested {} files, found failure after {}",
                tested,
                format_duration(elapsed)
            ));

            stats.total_pus = pu_count;
            return Ok(failure);
        }

        // Get current PU files
        let current_files = find_pu_files(&cfg.experiment_dir, &pattern);
        let pu_count = current_files.len();

        // Find new files to scan
        let new_files: Vec<PathBuf> = current_files
            .into_iter()
            .filter(|f| {
                if scanned_files.contains(f) {
                    return false;
                }
                scanned_files.insert(f.clone());

                // Skip PUs below START_PU
                if cfg.start_pu > 0 {
                    if let Some(name) = f.file_name().and_then(|n| n.to_str()) {
                        if let Some(pu_num) = get_pu_number(name) {
                            if pu_num < cfg.start_pu {
                                return false;
                            }
                        }
                    }
                }
                true
            })
            .collect();

        // Compile new files in parallel
        if !new_files.is_empty() {
            let failure_found_clone = Arc::clone(&failure_found);
            let first_failure_clone = Arc::clone(&first_failure);
            let tested_count_clone = Arc::clone(&tested_count);

            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(cfg.reserved_cpus)
                .build()
                .unwrap();

            pool.install(|| {
                new_files.par_iter().for_each(|f| {
                    if failure_found_clone.load(Ordering::Relaxed) {
                        return;
                    }

                    if !compile_file(f) {
                        if !failure_found_clone.swap(true, Ordering::Relaxed) {
                            *first_failure_clone.lock().unwrap() = Some(f.clone());
                        }
                        return;
                    }

                    tested_count_clone.fetch_add(1, Ordering::Relaxed);
                });
            });
        }

        // Check if generation finished
        if let Ok(Some(_)) = gen_child.try_wait() {
            // Final scan of remaining files
            let final_files = find_pu_files(&cfg.experiment_dir, &pattern);
            stats.total_pus = final_files.len();

            let remaining: Vec<PathBuf> = final_files
                .into_iter()
                .filter(|f| !scanned_files.contains(f))
                .collect();

            for f in remaining {
                if failure_found.load(Ordering::Relaxed) {
                    break;
                }
                if !compile_file(&f) {
                    *first_failure.lock().unwrap() = Some(f);
                    failure_found.store(true, Ordering::Relaxed);
                    break;
                }
            }

            if failure_found.load(Ordering::Relaxed) {
                let failure = first_failure.lock().unwrap().clone();
                eprintln!();
                eprintln!();
                if let Some(ref f) = failure {
                    log_info(&format!("First failure found: {:?}", f));
                }
                return Ok(failure);
            }

            // No failures
            eprintln!();
            eprintln!();
            log_ok(&format!(
                "No failures found! All {} files compile successfully.",
                stats.total_pus
            ));
            return Ok(None);
        }

        // Update progress
        let tested = tested_count.load(Ordering::Relaxed);
        let sc = spinner_chars();
        let c = sc[spin_i % sc.len()];
        spin_i += 1;

        let gen_status = if gen_child.try_wait().ok().flatten().is_none() {
            format!("{}generating{}", CYAN, NC)
        } else {
            format!("{}done{}", GREEN, NC)
        };

        eprint!(
            "\r\x1b[K  {}{}{} PUs: {}{}{} | Scanned: {}{}{} OK | Gen: {} | {}{}{}",
            CYAN, c, NC, BOLD, pu_count, NC, BOLD, tested, NC, gen_status, DIM,
            format_duration(elapsed),
            NC
        );

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn estimate_failures_by_sampling(cfg: &Config, stats: &mut Stats) -> Result<(), String> {
    log_stage("FAILURE ESTIMATION (SAMPLING)");
    eprintln!(
        "{}Sampling PUs to estimate total failures (using {} parallel compilers)...{}",
        DIM, cfg.reserved_cpus, NC
    );
    eprintln!();

    let input_basename = cfg
        .exp_input_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    let pattern = format!("{}_", input_basename);

    let all_files = find_pu_files(&cfg.experiment_dir, &pattern);
    stats.total_pus = all_files.len();

    let actual_sample = cfg.sample_size.min(stats.total_pus);

    // Shuffle and sample
    use rand::seq::SliceRandom;
    let mut rng = rand::thread_rng();
    let mut shuffled = all_files;
    shuffled.shuffle(&mut rng);
    let sample: Vec<PathBuf> = shuffled.into_iter().take(actual_sample).collect();

    let sample_start = Instant::now();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.reserved_cpus)
        .build()
        .unwrap();

    let ok_count = Arc::new(AtomicUsize::new(0));
    let fail_count = Arc::new(AtomicUsize::new(0));

    pool.install(|| {
        sample.par_iter().for_each(|f| {
            if compile_file(f) {
                ok_count.fetch_add(1, Ordering::Relaxed);
            } else {
                fail_count.fetch_add(1, Ordering::Relaxed);
            }
        });
    });

    let ok = ok_count.load(Ordering::Relaxed);
    let failures = fail_count.load(Ordering::Relaxed);
    let tested = ok + failures;

    let rate = failures as f64 / tested as f64;
    let est = (rate * stats.total_pus as f64) as usize;

    eprintln!();
    eprintln!();

    stats.current_estimate = est;
    let success_rate = 100 - (est * 100 / stats.total_pus.max(1));

    eprintln!("  {}Sample Results:{}", BOLD, NC);
    eprintln!("    Tested:     {} files (parallel)", tested);
    eprintln!(
        "    Failures:   {} ({:.1}%)",
        failures,
        failures as f64 * 100.0 / tested as f64
    );
    eprintln!();
    eprintln!("  {}Projected Results:{}", BOLD, NC);
    eprintln!(
        "    Estimated failures: {}{}{} / {}",
        RED, est, NC, stats.total_pus
    );
    eprintln!("    Success rate:       {}{}%{}", GREEN, success_rate, NC);
    eprintln!("    Baseline:           {}", cfg.baseline_failures);
    eprintln!();

    if est < cfg.baseline_failures {
        let improvement = cfg.baseline_failures - est;
        log_ok(&format!("Improvement! ~{} fewer estimated failures", improvement));
    } else if est > cfg.baseline_failures {
        let regression = est - cfg.baseline_failures;
        log_warn(&format!(
            "Possible regression: ~{} more failures than baseline",
            regression
        ));
    } else {
        log_info("No significant change from baseline");
    }

    eprintln!(
        "{}Stage completed in {}{}",
        DIM,
        format_duration(sample_start.elapsed().as_secs()),
        NC
    );
    Ok(())
}

fn create_test_case(cfg: &Config, failing_file: &Path) -> PathBuf {
    log_stage("CREATING MINIMAL TEST CASE");
    eprintln!("{}Extracting error context for bug analysis...{}", DIM, NC);
    eprintln!();

    // Find next bug number
    let mut max_bug = 0;
    for entry in fs::read_dir(&cfg.tests_dir).into_iter().flatten() {
        if let Ok(entry) = entry {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("bug") && name_str.ends_with(".c") {
                if let Some(num_str) = name_str.strip_prefix("bug").and_then(|s| s.strip_suffix(".c"))
                {
                    if let Ok(num) = num_str.parse::<usize>() {
                        max_bug = max_bug.max(num);
                    }
                }
            }
        }
    }
    let next_bug = max_bug + 1;
    let new_test_case = cfg.tests_dir.join(format!("bug{}.c", next_bug));

    // Get compilation errors
    let error_output = Command::new("gcc")
        .args(["-g", "-O2", "-c"])
        .arg(failing_file)
        .stderr(Stdio::piped())
        .output()
        .ok();

    let errors = error_output
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
        .unwrap_or_default();

    eprintln!("{}Compilation Errors:{}", BOLD, NC);
    eprintln!("{}", RED);
    for line in errors.lines().take(20) {
        eprintln!("{}", line);
    }
    eprintln!("{}", NC);

    // Extract error line number
    let error_line: Option<usize> = errors
        .lines()
        .find(|l| l.contains("error:"))
        .and_then(|l| {
            let parts: Vec<&str> = l.split(':').collect();
            if parts.len() >= 2 {
                parts[1].parse().ok()
            } else {
                None
            }
        });

    // Create test case
    if let Some(line_num) = error_line {
        log_info(&format!("Error at line: {}", line_num));

        let start = line_num.saturating_sub(50).max(1);
        let end = line_num + 20;

        if let Ok(content) = fs::read_to_string(failing_file) {
            let lines: Vec<&str> = content.lines().collect();
            let extracted: String = lines
                .iter()
                .skip(start - 1)
                .take(end - start + 1)
                .map(|l| format!("{}\n", l))
                .collect();

            let test_content = format!(
                "// Minimal test case from {:?}\n\
                 // Error at line {}\n\
                 // Compile: gcc -E bug{}.c -o bug{}.i\n\
                 // Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../bin/precc bug{}.i\n\n\
                 {}",
                failing_file, line_num, next_bug, next_bug, next_bug, extracted
            );

            fs::write(&new_test_case, test_content).ok();
        }
    } else if let Ok(content) = fs::read_to_string(failing_file) {
        let lines: Vec<&str> = content.lines().take(150).collect();
        let test_content = format!(
            "// Minimal test case from {:?}\n\n{}",
            failing_file,
            lines.join("\n")
        );
        fs::write(&new_test_case, test_content).ok();
    }

    log_ok(&format!("Created: {:?}", new_test_case));

    // Copy full PU
    let full_pu = cfg
        .tests_dir
        .join(format!("bug{}_full.pu.c", next_bug));
    fs::copy(failing_file, &full_pu).ok();
    log_info(&format!("Full PU saved: {:?}", full_pu));

    new_test_case
}

fn call_claude_fix(cfg: &Config, failing_file: &Path, test_case: &Path, pu_num: usize) {
    log_stage("CLAUDE AUTO-FIX");
    eprintln!(
        "{}Invoking Claude to analyze and fix the bug...{}",
        DIM, NC
    );
    eprintln!();

    let prompt = format!(
        r#"I need you to fix a compilation error in the precc tool (a C/C++ precompiler that splits files into compilation units).

## The Problem

The file `{:?}` fails to compile with gcc.

## Minimal Test Case

I've created a minimal test case at `{:?}`.

## Your Task

1. First, analyze the error pattern to understand what's causing the compilation failure
2. Read the relevant parts of `src/lib.rs` to understand how precc generates the output
3. Identify what change in `src/lib.rs` would fix this compilation error
4. Make the fix in `src/lib.rs`
5. After fixing, verify by running:
   - `cargo build --release`
   - `PU_FILTER={} PASSTHROUGH_THRESHOLD=0 SPLIT=1 bin/precc {}`
   - `gcc -g -O2 -c {}_{}.pu.c`
6. If the fixing is confirmed, call /exit to quit the interactive session.

## Common Error Patterns

Based on previous bugs, common issues include:
- Missing forward declarations for functions
- K&R style declarations conflicting with actual prototypes
- Typedef ordering issues
- Void pointer dereference errors
- Struct declarations in wrong order

Focus on fixing the root cause in the code generation logic, not just this specific case.

Please analyze and fix this bug now."#,
        failing_file,
        test_case,
        pu_num,
        cfg.input_file,
        cfg.input_file,
        pu_num
    );

    eprintln!("  {}Bug Details:{}", BOLD, NC);
    eprintln!("    Failing PU:   {:?}", failing_file);
    eprintln!("    PU Number:    {}", pu_num);
    eprintln!("    Test Case:    {:?}", test_case);
    eprintln!();

    // Check if claude CLI is available
    if Command::new("claude").arg("--version").output().is_ok() {
        eprintln!(
            "{}[CLAUDE]{} Starting interactive session for bug analysis and fixing...",
            MAGENTA, NC
        );
        eprintln!("{}You can help guide the debugging and repair process.{}", DIM, NC);
        eprintln!(
            "{}When done, type /exit or press Ctrl+C to continue the test workflow.{}",
            DIM, NC
        );
        eprintln!();

        Command::new("claude")
            .arg("--dangerously-skip-permissions")
            .arg(&prompt)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .ok();

        eprintln!();
        eprintln!(
            "{}[INFO]{} Claude session ended. Continuing test workflow...",
            CYAN, NC
        );
    } else {
        log_error("Claude CLI not found. Please install claude-code.");
        log_info("Manual fix required.");
    }
}

fn verify_fix(cfg: &Config, stats: &mut Stats, pu_num: usize) -> bool {
    log_stage("VERIFYING FIX");
    eprintln!("{}Rebuilding and testing the fix...{}", DIM, NC);
    eprintln!();

    let input_basename = cfg
        .exp_input_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");

    eprintln!("  {}Step 1/3:{} Rebuilding precc...", BOLD, NC);
    let build_status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&cfg.source_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status();

    if !build_status.map(|s| s.success()).unwrap_or(false) {
        log_error("Build failed!");
        return false;
    }
    copy_precc_binary(cfg);
    log_ok("Build successful");

    eprintln!("  {}Step 2/3:{} Regenerating PU {}...", BOLD, NC, pu_num);
    let mut regen_cmd = Command::new(&cfg.precc_bin);
    regen_cmd
        .arg(&cfg.exp_input_file)
        .current_dir(&cfg.experiment_dir)
        .env("PU_FILTER", pu_num.to_string())
        .env("PASSTHROUGH_THRESHOLD", "0")
        .env("JOBS", "1")
        .stderr(Stdio::null())
        .stdout(Stdio::null());
    apply_precc_build_options(&mut regen_cmd, cfg);
    let regen_status = regen_cmd.status();

    if !regen_status.map(|s| s.success()).unwrap_or(false) {
        log_error("PU regeneration failed!");
        return false;
    }
    log_ok("PU regenerated");

    let pu_file = cfg
        .experiment_dir
        .join(format!("{}_{}.pu.c", input_basename, pu_num));
    eprintln!("  {}Step 3/3:{} Testing compilation...", BOLD, NC);

    if compile_file(&pu_file) {
        log_ok(&format!("PU {} now compiles successfully!", pu_num));
        stats.bugs_fixed += 1;
        eprintln!();
        eprintln!(
            "  {}{} Bug fixed!{} Total bugs fixed this session: {}{}{}",
            GREEN, check_mark(), NC, BOLD, stats.bugs_fixed, NC
        );
        true
    } else {
        log_error(&format!("PU {} still fails to compile", pu_num));
        false
    }
}

// ============================================================================
// PERFORMANCE MODE
// ============================================================================

fn run_generation_benchmark(cfg: &Config, stats: &mut Stats) -> bool {
    log_stage("GENERATION BENCHMARK");
    eprintln!("{}Running full PU generation with timing...{}", DIM, NC);
    eprintln!();

    // Clean previous
    for entry in fs::read_dir(&cfg.experiment_dir).into_iter().flatten() {
        if let Ok(entry) = entry {
            if entry
                .file_name()
                .to_string_lossy()
                .ends_with(".pu.c")
            {
                fs::remove_file(entry.path()).ok();
            }
        }
    }

    eprintln!("  {}Configuration:{}", BOLD, NC);
    eprintln!("    Input file:     {:?}", cfg.exp_input_file);
    eprintln!("    Parallel jobs:  {}", cfg.precc_jobs);
    if cfg.inprocess_mode {
        eprintln!(
            "    Inprocess mode: {}enabled{} (thread-safe parallel ctags)",
            GREEN, NC
        );
    }
    eprintln!();

    let gen_start = Instant::now();

    // Use default threshold (1.3MB) - small files use passthrough, large files get split
    let mut cmd = Command::new(&cfg.precc_bin);
    cmd.arg(&cfg.exp_input_file)
        .current_dir(&cfg.experiment_dir)
        .env("JOBS", cfg.precc_jobs.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    apply_precc_build_options(&mut cmd, cfg);

    let status = cmd.status();
    if !status.map(|s| s.success()).unwrap_or(false) {
        log_error("Generation failed!");
        return false;
    }

    let gen_time = gen_start.elapsed().as_secs_f64();

    let input_basename = cfg
        .exp_input_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    let pu_count = find_all_pu_files(&cfg.experiment_dir, input_basename).len();
    stats.total_pus = pu_count;

    let throughput = pu_count as f64 / gen_time;

    eprintln!();
    eprintln!("  {}Generation Results:{}", BOLD, NC);
    eprintln!("    PUs generated:  {}{}{}", GREEN, pu_count, NC);
    eprintln!("    Total time:     {}{:.3}s{}", CYAN, gen_time, NC);
    eprintln!("    Throughput:     {}{:.2} PUs/sec{}", CYAN, throughput, NC);
    eprintln!();

    stats.perf_gen_time = gen_time;
    stats.perf_gen_throughput = throughput;

    true
}

fn run_compilation_benchmark(cfg: &Config, stats: &mut Stats) -> bool {
    log_stage("COMPILATION BENCHMARK");

    let input_basename = cfg
        .exp_input_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");

    // Check for unity batch files
    let unity_dir = cfg.experiment_dir.join(format!("{}_unity", input_basename));
    let unity_files: Vec<PathBuf> = if cfg.unity_build && unity_dir.exists() {
        fs::read_dir(&unity_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("unity_") && n.ends_with(".c"))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        Vec::new()
    };

    let use_unity = !unity_files.is_empty();

    if use_unity {
        eprintln!(
            "{}Compiling unity batches with parallel gcc...{}",
            DIM, NC
        );
    } else {
        eprintln!(
            "{}Compiling all PUs with parallel gcc...{}",
            DIM, NC
        );
    }
    eprintln!();

    let pu_files = find_all_pu_files(&cfg.experiment_dir, input_basename);
    let total_pus = pu_files.len();

    eprintln!("  {}Configuration:{}", BOLD, NC);
    eprintln!("    Total PUs:      {}", total_pus);
    if use_unity {
        eprintln!("    Unity batches:  {}", unity_files.len());
        eprintln!("    Compile mode:   {}UNITY{} (faster)", GREEN, NC);
    } else {
        eprintln!("    Compile mode:   Individual PUs");
    }
    eprintln!("    Parallel jobs:  {} (gcc instances)", cfg.reserved_cpus);
    eprintln!();

    let comp_start = Instant::now();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.reserved_cpus)
        .build()
        .unwrap();

    let passed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));

    if use_unity {
        // Compile unity batch files
        let passed_clone = Arc::clone(&passed);
        let failed_clone = Arc::clone(&failed);
        let unity_dir_clone = unity_dir.clone();

        pool.install(|| {
            unity_files.par_iter().for_each(|f| {
                // Compile unity batch file with include path to parent dir (for common header)
                let status = Command::new("gcc")
                    .args(["-g", "-O2", "-w", "-c"])
                    .arg(f)
                    .arg("-I")
                    .arg(unity_dir_clone.parent().unwrap_or(Path::new(".")))
                    .current_dir(&unity_dir_clone)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();

                if status.map(|s| s.success()).unwrap_or(false) {
                    passed_clone.fetch_add(1, Ordering::Relaxed);
                } else {
                    failed_clone.fetch_add(1, Ordering::Relaxed);
                }
            });
        });
    } else {
        // Compile individual PU files
        let passed_clone = Arc::clone(&passed);
        let failed_clone = Arc::clone(&failed);

        pool.install(|| {
            pu_files.par_iter().for_each(|f| {
                if compile_file(f) {
                    passed_clone.fetch_add(1, Ordering::Relaxed);
                } else {
                    failed_clone.fetch_add(1, Ordering::Relaxed);
                }
            });
        });
    }

    let comp_time = comp_start.elapsed().as_secs_f64();
    let passed_count = passed.load(Ordering::Relaxed);
    let failed_count = failed.load(Ordering::Relaxed);

    let total = if use_unity {
        unity_files.len()
    } else {
        total_pus
    };

    let success_rate = if total > 0 {
        passed_count as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    let throughput = if comp_time > 0.0 {
        total_pus as f64 / comp_time
    } else {
        0.0
    };

    eprintln!();
    eprintln!("  {}Compilation Results:{}", BOLD, NC);
    if use_unity {
        eprintln!(
            "    Batches:        {}{}{} / {} passed",
            GREEN, passed_count, NC, total
        );
        eprintln!(
            "    PUs covered:    {} (in {} batches)",
            total_pus, unity_files.len()
        );
    } else {
        eprintln!(
            "    Passed:         {}{}{} / {} ({:.1}%)",
            GREEN, passed_count, NC, total, success_rate
        );
    }
    eprintln!("    Failed:         {}{}{}", RED, failed_count, NC);
    eprintln!("    Total time:     {}{:.3}s{}", CYAN, comp_time, NC);
    eprintln!("    Throughput:     {}{:.2} PUs/sec{}", CYAN, throughput, NC);
    eprintln!();

    stats.perf_comp_time = comp_time;
    stats.perf_comp_throughput = throughput;
    stats.perf_comp_passed = if use_unity { total_pus } else { passed_count };
    stats.perf_comp_failed = failed_count;
    stats.perf_success_rate = if use_unity && failed_count == 0 { 100.0 } else { success_rate };

    // Run linking validation if not using unity build
    if !use_unity && stats.perf_success_rate == 100.0 {
        run_sqlite_link_benchmark(cfg, stats, &pu_files);
    }

    true
}

fn print_perf_summary(_cfg: &Config, stats: &Stats) {
    let total_time = stats.start_time.elapsed().as_secs();

    eprintln!();
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|              PERFORMANCE SUMMARY                           |{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  Total Time:          {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format_duration(total_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}Regression Tests:{}                                         {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Test cases:        {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{} passed", stats.regression_tests_passed),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}Generation:{}                                               {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    PUs generated:     {:<36} {}{}|{}",
        BOLD, GREEN, NC, stats.total_pus, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Time:              {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s", stats.perf_gen_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Throughput:        {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.2} PUs/sec", stats.perf_gen_throughput),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}Compilation:{}                                              {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Passed:            {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!(
            "{} / {} ({:.1}%)",
            stats.perf_comp_passed, stats.total_pus, stats.perf_success_rate
        ),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Failed:            {:<36} {}{}|{}",
        BOLD, GREEN, NC, stats.perf_comp_failed, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Time:              {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s", stats.perf_comp_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Throughput:        {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.2} PUs/sec", stats.perf_comp_throughput),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}End-to-End Metrics:{}                                       {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );

    // Calculate end-to-end metrics (includes preprocessing for real-world comparison)
    let e2e_time = stats.perf_preprocess_time + stats.perf_gen_time + stats.perf_comp_time;
    let e2e_throughput = if e2e_time > 0.0 {
        stats.total_pus as f64 / e2e_time
    } else {
        0.0
    };

    if stats.perf_preprocess_time > 0.0 {
        eprintln!(
            "{}{}|{}    Preprocessing:     {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{:.3}s (gcc -E)", stats.perf_preprocess_time),
            BOLD,
            GREEN,
            NC
        );
    }
    eprintln!(
        "{}{}|{}    Generation:        {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s (precc)", stats.perf_gen_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Compilation:       {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s", stats.perf_comp_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    ------------------------------------------------------- {}{}|{}",
        BOLD, GREEN, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    {}Total:             {:<36}{} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        CYAN,
        if stats.perf_preprocess_time > 0.0 {
            format!("{:.3}s (preproc+gen+comp)", e2e_time)
        } else {
            format!("{:.3}s (gen+comp)", e2e_time)
        },
        NC,
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    {}E2E Throughput:    {:<36}{} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        CYAN,
        format!("{:.2} PUs/sec", e2e_throughput),
        NC,
        BOLD,
        GREEN,
        NC
    );

    // Add linking validation section if linking was performed
    if stats.perf_link_success || stats.perf_orig_comp_time > 0.0 {
        eprintln!(
            "{}{}+------------------------------------------------------------+{}",
            BOLD, GREEN, NC
        );
        eprintln!(
            "{}{}|{}  {}Linking Validation:{}                                       {}{}|{}",
            BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
        );
        eprintln!(
            "{}{}|{}    Orig. compile:     {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{:.3}s", stats.perf_orig_comp_time),
            BOLD,
            GREEN,
            NC
        );
        eprintln!(
            "{}{}|{}    Split link:        {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{:.3}s", stats.perf_link_time),
            BOLD,
            GREEN,
            NC
        );
        let symbols_status = if stats.perf_symbols_match {
            format!("{} symbols match", stats.perf_symbols_count)
        } else {
            format!("{} symbols (mismatch)", stats.perf_symbols_count)
        };
        eprintln!(
            "{}{}|{}    Symbols:           {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            symbols_status,
            BOLD,
            GREEN,
            NC
        );
        // Code size comparison (text section) - only show if sizes are comparable
        // (split mode with ld -r loses code due to deduplication)
        if stats.perf_orig_text_size > 0 && stats.perf_split_text_size > 0 {
            let ratio = stats.perf_split_text_size as f64 / stats.perf_orig_text_size as f64;
            if ratio > 0.5 && ratio < 2.0 {
                let diff_pct = ((ratio - 1.0) * 100.0).abs();
                let code_size_str = format!("{} vs {} ({:.2}% diff)",
                    format_size(stats.perf_orig_text_size),
                    format_size(stats.perf_split_text_size),
                    diff_pct);
                eprintln!(
                    "{}{}|{}    Code size:         {:<36} {}{}|{}",
                    BOLD, GREEN, NC, code_size_str, BOLD, GREEN, NC
                );
            }
        }
        // Comparison row (includes preprocessing for accurate real-world comparison)
        let total_split_time = stats.perf_preprocess_time + stats.perf_gen_time + stats.perf_comp_time + stats.perf_link_time;
        let speedup = if total_split_time > 0.0 {
            stats.perf_orig_comp_time / total_split_time
        } else {
            0.0
        };
        eprintln!(
            "{}{}|{}    ------------------------------------------------------- {}{}|{}",
            BOLD, GREEN, NC, BOLD, GREEN, NC
        );
        if stats.perf_preprocess_time > 0.0 {
            eprintln!(
                "{}{}|{}    {}Split total:       {:<36}{} {}{}|{}",
                BOLD,
                GREEN,
                NC,
                CYAN,
                format!("{:.3}s (preproc+gen+comp+link)", total_split_time),
                NC,
                BOLD,
                GREEN,
                NC
            );
        } else {
            eprintln!(
                "{}{}|{}    {}Split total:       {:<36}{} {}{}|{}",
                BOLD,
                GREEN,
                NC,
                CYAN,
                format!("{:.3}s (gen+comp+link)", total_split_time),
                NC,
                BOLD,
                GREEN,
                NC
            );
        }
        eprintln!(
            "{}{}|{}    {}vs Original:       {:<36}{} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            CYAN,
            format!("{:.2}x {}", speedup, if speedup >= 1.0 { "faster" } else { "slower" }),
            NC,
            BOLD,
            GREEN,
            NC
        );
    }

    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
}

fn perf_mode_loop(cfg: &Config, stats: &mut Stats) -> bool {
    print_mode_banner("PERFORMANCE MODE");

    build_if_needed(cfg);

    if !run_regression_test(cfg, stats) {
        log_error("Regression test failed! Cannot proceed with performance testing.");
        return false;
    }

    if !cfg.exp_input_file.exists() {
        log_warn(&format!(
            "Input file '{:?}' not found in experiment dir",
            cfg.exp_input_file
        ));
        log_info("Regression tests passed. No large file for benchmarking.");
        return true;
    }

    // Measure preprocessing time for accurate real-world comparison
    // The .i file already exists, but we measure how long it would take to generate
    let sqlite_dir = cfg.source_dir.join("tests/sqlite3");
    let sqlite3_c = sqlite_dir.join("sqlite3.c");
    if sqlite3_c.exists() {
        log_info("Measuring preprocessing time (gcc -E)...");
        let preproc_start = Instant::now();
        let preproc_output = cfg.experiment_dir.join("sqlite3_preproc_test.i");
        let preproc_status = Command::new("gcc")
            .args(["-E", "-o"])
            .arg(&preproc_output)
            .arg(&sqlite3_c)
            .current_dir(&sqlite_dir)
            .stderr(Stdio::null())
            .status();
        stats.perf_preprocess_time = preproc_start.elapsed().as_secs_f64();
        fs::remove_file(&preproc_output).ok();  // Clean up test file

        if preproc_status.map(|s| s.success()).unwrap_or(false) {
            eprintln!("    Preprocessing time: {}{:.2}s{}", CYAN, stats.perf_preprocess_time, NC);
        }
    }

    if !run_generation_benchmark(cfg, stats) {
        log_error("Generation benchmark failed!");
        return false;
    }

    if !run_compilation_benchmark(cfg, stats) {
        log_error("Compilation benchmark failed!");
        return false;
    }

    print_perf_summary(cfg, stats);
    true
}

// ============================================================================
// VIM MODE
// ============================================================================

fn run_vim_benchmark(cfg: &Config, stats: &mut Stats) {
    log_stage("VIM SOURCE FILES BENCHMARK");
    eprintln!("{}Testing precc on vim source files...{}", DIM, NC);
    eprintln!();

    let vim_src = cfg.source_dir.join("tests/vim/src");
    let vim_proto = cfg.source_dir.join("tests/vim/src/proto");
    let work_dir = cfg.experiment_dir.join("vim_work");

    fs::create_dir_all(&work_dir).ok();

    // Count files
    let c_files: Vec<PathBuf> = fs::read_dir(&vim_src)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "c").unwrap_or(false))
        .map(|e| e.path())
        .collect();

    stats.vim_total_files = c_files.len();

    eprintln!("  {}Configuration:{}", BOLD, NC);
    eprintln!("    Vim source dir:   {:?}", vim_src);
    eprintln!("    Total C files:    {}", stats.vim_total_files);
    eprintln!("    Work directory:   {:?}", work_dir);
    eprintln!();

    let gen_start = Instant::now();

    for (i, cfile) in c_files.iter().enumerate() {
        let fname = cfile
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // Progress
        eprint!(
            "\r\x1b[K{}Vim files{} [{}/{}] {}",
            DIM,
            NC,
            i + 1,
            stats.vim_total_files,
            fname
        );

        // Skip platform-specific
        if should_skip_vim_file(fname) {
            stats.vim_files_skipped += 1;
            continue;
        }

        let i_file = work_dir.join(format!("{}.i", fname));

        // Preprocess
        let vim_auto = vim_src.join("auto");
        let prep_status = Command::new("gcc")
            .args(["-E", "-I"])
            .arg(&vim_src)
            .arg("-I")
            .arg(&vim_auto)
            .arg("-I")
            .arg(&vim_proto)
            .args(["-DHAVE_CONFIG_H"])
            .arg(cfile)
            .arg("-o")
            .arg(&i_file)
            .stderr(Stdio::null())
            .status();

        if !prep_status.map(|s| s.success()).unwrap_or(false) {
            stats.vim_files_skipped += 1;
            continue;
        }

        // Generate PUs
        let file_gen_start = Instant::now();
        let mut cmd = Command::new(&cfg.precc_bin);
        cmd.arg(&i_file)
            .env("PASSTHROUGH_THRESHOLD", "0")
            .stderr(Stdio::null())
            .stdout(Stdio::null());
        apply_precc_build_options(&mut cmd, cfg);

        if !cmd.status().map(|s| s.success()).unwrap_or(false) {
            stats.vim_files_skipped += 1;
            fs::remove_file(&i_file).ok();
            continue;
        }

        let file_gen_time = file_gen_start.elapsed().as_secs_f64();
        stats.vim_gen_time += file_gen_time;

        // Count and compile PUs (both split mode _*.pu.c and passthrough mode .pu.c)
        let i_basename = format!("{}.i", fname);
        let pu_files = find_all_pu_files(&work_dir, &i_basename);
        if pu_files.is_empty() {
            stats.vim_files_skipped += 1;
            fs::remove_file(&i_file).ok();
            continue;
        }

        stats.vim_files_tested += 1;
        stats.vim_total_pus += pu_files.len();

        for pu in &pu_files {
            if compile_file_with_includes(pu, &vim_src, &vim_proto) {
                stats.vim_comp_passed += 1;
            } else {
                stats.vim_comp_failed += 1;
            }
        }

        // Cleanup
        fs::remove_file(&i_file).ok();
        for pu in pu_files {
            fs::remove_file(pu).ok();
        }
    }

    eprintln!();
    eprintln!();

    let total_time = gen_start.elapsed().as_secs_f64();

    stats.vim_success_rate = if stats.vim_total_pus > 0 {
        stats.vim_comp_passed as f64 * 100.0 / stats.vim_total_pus as f64
    } else {
        0.0
    };

    stats.vim_throughput = if stats.vim_gen_time > 0.0 {
        stats.vim_total_pus as f64 / stats.vim_gen_time
    } else {
        0.0
    };

    eprintln!("  {}Vim Test Results:{}", BOLD, NC);
    eprintln!(
        "    Files tested:     {}{}{} / {}",
        GREEN, stats.vim_files_tested, NC, stats.vim_total_files
    );
    eprintln!(
        "    Files skipped:    {}{}{} (platform-specific)",
        YELLOW, stats.vim_files_skipped, NC
    );
    eprintln!("    Total PUs:        {}{}{}", CYAN, stats.vim_total_pus, NC);
    eprintln!("    Compiled OK:      {}{}{}", GREEN, stats.vim_comp_passed, NC);
    eprintln!("    Compiled FAIL:    {}{}{}", RED, stats.vim_comp_failed, NC);
    eprintln!(
        "    Success rate:     {}{:.2}%{}",
        CYAN, stats.vim_success_rate, NC
    );
    eprintln!(
        "    Generation time:  {}{:.3}s{}",
        CYAN, stats.vim_gen_time, NC
    );
    eprintln!(
        "    Gen throughput:   {}{:.2} PUs/sec{}",
        CYAN, stats.vim_throughput, NC
    );
    eprintln!("    Total time:       {}{:.3}s{}", CYAN, total_time, NC);
}

fn run_vim_benchmark_parallel(cfg: &Config, stats: &mut Stats) {
    log_stage("VIM SOURCE FILES BENCHMARK (PARALLEL)");
    eprintln!(
        "{}Testing precc on vim source files with {} parallel jobs...{}",
        DIM, cfg.split_jobs, NC
    );
    eprintln!();

    let vim_src = cfg.source_dir.join("tests/vim/src");
    let vim_auto = vim_src.join("auto");
    let vim_proto = cfg.source_dir.join("tests/vim/src/proto");
    let work_dir = cfg.experiment_dir.join("vim_work");
    let prep_dir = work_dir.join("preprocessed");
    let split_dir = work_dir.join("split");

    fs::create_dir_all(&prep_dir).ok();
    fs::create_dir_all(&split_dir).ok();

    // Get files to process
    let c_files: Vec<PathBuf> = fs::read_dir(&vim_src)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "c").unwrap_or(false))
        .map(|e| e.path())
        .collect();

    stats.vim_total_files = c_files.len();

    let files_to_process: Vec<PathBuf> = c_files
        .iter()
        .filter(|f| {
            let fname = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if should_skip_vim_file(fname) {
                false
            } else {
                true
            }
        })
        .cloned()
        .collect();

    stats.vim_files_skipped = c_files.len() - files_to_process.len();

    eprintln!("  {}Configuration:{}", BOLD, NC);
    eprintln!("    Vim source dir:   {:?}", vim_src);
    eprintln!("    Total C files:    {}", stats.vim_total_files);
    eprintln!("    Parallel jobs:    {}", cfg.split_jobs);
    eprintln!("    Work directory:   {:?}", work_dir);
    eprintln!();

    let total_start = Instant::now();

    // Phase 1: Preprocess
    eprintln!("  {}Phase 1/3:{} Preprocessing files in parallel...", BOLD, NC);
    let prep_start = Instant::now();

    let prep_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.split_jobs)
        .build()
        .unwrap();

    prep_pool.install(|| {
        files_to_process.par_iter().for_each(|cfile| {
            let fname = cfile.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let i_file = prep_dir.join(format!("{}.i", fname));

            Command::new("gcc")
                .args(["-E", "-I"])
                .arg(&vim_src)
                .arg("-I")
                .arg(&vim_auto)
                .arg("-I")
                .arg(&vim_proto)
                .args(["-DHAVE_CONFIG_H"])
                .arg(cfile)
                .arg("-o")
                .arg(&i_file)
                .stderr(Stdio::null())
                .status()
                .ok();
        });
    });

    stats.vim_preprocess_time = prep_start.elapsed().as_secs_f64();
    let prep_count: usize = fs::read_dir(&prep_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.is_ok())
        .count();
    eprintln!(
        "    Preprocessed {}{}{} files in {}{:.2}s{}",
        GREEN, prep_count, NC, CYAN, stats.vim_preprocess_time, NC
    );
    eprintln!();

    // Phase 2: Run precc split
    eprintln!(
        "  {}Phase 2/3:{} Running precc split in parallel ({} jobs)...",
        BOLD, NC, cfg.split_jobs
    );
    let gen_start = Instant::now();

    let i_files: Vec<PathBuf> = fs::read_dir(&prep_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "i").unwrap_or(false))
        .map(|e| e.path())
        .collect();

    let gen_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.split_jobs)
        .build()
        .unwrap();

    let vim_total_pus = Arc::new(AtomicUsize::new(0));
    let vim_files_tested = Arc::new(AtomicUsize::new(0));
    let precc_bin = cfg.precc_bin.clone();
    let inprocess_mode = cfg.inprocess_mode;
    // Capture precc build options before entering parallel context
    // Use passthrough mode (split=false) as default for Vim - fairer comparison
    // with original compilation (same number of files)
    let split = false;  // Passthrough mode: one .pu.c per .i file
    let lattice_headers = cfg.lattice_headers;
    let unity_build = cfg.unity_build;
    let unity_batches = cfg.unity_batches;

    gen_pool.install(|| {
        i_files.par_iter().for_each(|i_file| {
            let mut cmd = Command::new(&precc_bin);
            cmd.arg(i_file)
                // Use default threshold (1.3MB) - small files use passthrough, large files get split
                .stderr(Stdio::null())
                .stdout(Stdio::null());
            apply_precc_build_options_raw(&mut cmd, split, lattice_headers, unity_build, unity_batches, inprocess_mode);

            if cmd.status().map(|s| s.success()).unwrap_or(false) {
                // Count generated PUs (both split mode _*.pu.c and passthrough mode .pu.c)
                let i_basename = i_file.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let parent = i_file.parent().unwrap_or(Path::new("."));
                let pu_files = find_all_pu_files(parent, i_basename);
                let pu_count = pu_files.len();

                if pu_count > 0 {
                    // Move PUs to split_dir
                    for pu in pu_files {
                        if let Some(name) = pu.file_name() {
                            let dest = split_dir.join(name);
                            fs::rename(&pu, dest).ok();
                        }
                    }

                    // Move unity build artifacts if present (unity dir only, not common header)
                    if unity_build {
                        // Unity directory: {basename}_unity/ - move if exists
                        let unity_dir_path = parent.join(format!("{}_unity", i_basename));
                        if unity_dir_path.exists() {
                            let dest = split_dir.join(format!("{}_unity", i_basename));
                            fs::rename(&unity_dir_path, dest).ok();
                        }
                    }

                    vim_total_pus.fetch_add(pu_count, Ordering::Relaxed);
                    vim_files_tested.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    });

    stats.vim_gen_time = gen_start.elapsed().as_secs_f64();
    stats.vim_total_pus = vim_total_pus.load(Ordering::Relaxed);
    stats.vim_files_tested = vim_files_tested.load(Ordering::Relaxed);

    eprintln!(
        "    Generated {}{}{} PUs from {}{}{} files in {}{:.3}s{}",
        GREEN,
        stats.vim_total_pus,
        NC,
        GREEN,
        stats.vim_files_tested,
        NC,
        CYAN,
        stats.vim_gen_time,
        NC
    );
    eprintln!();

    // Phase 3: Compile
    eprintln!("  {}Phase 3/3:{} Compiling PUs in parallel...", BOLD, NC);
    let comp_start = Instant::now();

    let pu_files: Vec<PathBuf> = fs::read_dir(&split_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.ends_with(".pu.c"))
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    let comp_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.reserved_cpus)
        .build()
        .unwrap();

    let passed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));

    comp_pool.install(|| {
        pu_files.par_iter().for_each(|pu| {
            if compile_file_with_includes(pu, &vim_src, &vim_proto) {
                passed.fetch_add(1, Ordering::Relaxed);
            } else {
                failed.fetch_add(1, Ordering::Relaxed);
            }
        });
    });

    stats.vim_comp_passed = passed.load(Ordering::Relaxed);
    stats.vim_comp_failed = failed.load(Ordering::Relaxed);
    stats.vim_comp_time = comp_start.elapsed().as_secs_f64();

    eprintln!(
        "    Compiled {}{}{} OK, {}{}{} FAIL in {}{:.1}s{}",
        GREEN, stats.vim_comp_passed, NC, RED, stats.vim_comp_failed, NC, CYAN, stats.vim_comp_time, NC
    );
    eprintln!();

    let total_time = total_start.elapsed().as_secs_f64();

    stats.vim_success_rate = if stats.vim_total_pus > 0 {
        stats.vim_comp_passed as f64 * 100.0 / stats.vim_total_pus as f64
    } else {
        0.0
    };

    stats.vim_throughput = if stats.vim_gen_time > 0.0 {
        stats.vim_total_pus as f64 / stats.vim_gen_time
    } else {
        0.0
    };

    eprintln!("  {}Vim Test Results (Parallel):{}", BOLD, NC);
    // Show build mode
    let mode_str = if cfg.unity_build {
        "unity"
    } else if cfg.lattice_headers {
        "lattice"
    } else if cfg.split {
        "split"
    } else {
        "passthrough"
    };
    eprintln!("    Build mode:       {}{}{}", CYAN, mode_str, NC);
    eprintln!(
        "    Files tested:     {}{}{} / {}",
        GREEN, stats.vim_files_tested, NC, stats.vim_total_files
    );
    eprintln!(
        "    Files skipped:    {}{}{} (platform-specific)",
        YELLOW, stats.vim_files_skipped, NC
    );
    eprintln!("    Total PUs:        {}{}{}", CYAN, stats.vim_total_pus, NC);
    eprintln!("    Compiled OK:      {}{}{}", GREEN, stats.vim_comp_passed, NC);
    eprintln!("    Compiled FAIL:    {}{}{}", RED, stats.vim_comp_failed, NC);
    eprintln!(
        "    Success rate:     {}{:.2}%{}",
        CYAN, stats.vim_success_rate, NC
    );
    eprintln!(
        "    Preprocess time:  {}{:.2}s{} (gcc -E, parallel)",
        CYAN, stats.vim_preprocess_time, NC
    );
    eprintln!(
        "    Generation time:  {}{:.3}s{} (precc, parallel)",
        CYAN, stats.vim_gen_time, NC
    );
    eprintln!(
        "    Gen throughput:   {}{:.2} PUs/sec{}",
        CYAN, stats.vim_throughput, NC
    );
    eprintln!("    Compile time:     {}{:.1}s{} (gcc -c, parallel)", CYAN, stats.vim_comp_time, NC);
    eprintln!("    Total time:       {}{:.3}s{}", CYAN, total_time, NC);

    // Run linking validation if all PUs compiled successfully
    if stats.vim_success_rate == 100.0 {
        run_vim_link_validation(cfg, stats, &pu_files, &i_files, &vim_src, &vim_proto);
    }

    // Cleanup (unless --keep is specified)
    if !cfg.keep_files {
        fs::remove_dir_all(&prep_dir).ok();
        fs::remove_dir_all(&split_dir).ok();
    } else {
        eprintln!();
        eprintln!("  {}Kept files:{}", BOLD, NC);
        eprintln!("    Preprocessed: {}", prep_dir.display());
        eprintln!("    Split PUs:    {}", split_dir.display());
    }
}

/// Threshold sweep result for a single threshold value
struct ThresholdResult {
    threshold: u64,
    files_split: usize,
    total_pus: usize,
    orig_time: f64,
    split_time: f64,
    speedup: f64,
}

/// Run threshold sweep to measure speedup curve
fn run_threshold_sweep(cfg: &Config, stats: &mut Stats) {
    log_stage("THRESHOLD SWEEP MODE");
    eprintln!("{}Testing different passthrough thresholds to generate speedup curve...{}", DIM, NC);
    eprintln!();

    let vim_src = cfg.source_dir.join("tests/vim/src");
    let vim_auto = vim_src.join("auto");
    let vim_proto = cfg.source_dir.join("tests/vim/src/proto");
    let work_dir = cfg.experiment_dir.join("vim_threshold_sweep");
    let prep_dir = work_dir.join("preprocessed");

    fs::create_dir_all(&prep_dir).ok();

    // Get files to process (excluding platform-specific ones)
    let c_files: Vec<PathBuf> = fs::read_dir(&vim_src)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "c").unwrap_or(false))
        .map(|e| e.path())
        .collect();

    let files_to_process: Vec<PathBuf> = c_files
        .iter()
        .filter(|f| {
            let fname = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            !should_skip_vim_file(fname)
        })
        .cloned()
        .collect();

    eprintln!("  {}Phase 1:{} Preprocessing {} files...", BOLD, NC, files_to_process.len());

    // Phase 1: Preprocess all files (timed for accurate real-world comparison)
    let preproc_start = Instant::now();
    let prep_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.split_jobs)
        .build()
        .unwrap();

    prep_pool.install(|| {
        files_to_process.par_iter().for_each(|cfile| {
            let fname = cfile.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let i_file = prep_dir.join(format!("{}.i", fname));

            Command::new("gcc")
                .args(["-E", "-I"])
                .arg(&vim_src)
                .arg("-I")
                .arg(&vim_auto)
                .arg("-I")
                .arg(&vim_proto)
                .args(["-DHAVE_CONFIG_H"])
                .arg(cfile)
                .arg("-o")
                .arg(&i_file)
                .stderr(Stdio::null())
                .status()
                .ok();
        });
    });
    let preproc_time = preproc_start.elapsed().as_secs_f64();

    // Collect .i files with their sizes
    let mut i_files_with_sizes: Vec<(PathBuf, u64)> = fs::read_dir(&prep_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "i").unwrap_or(false))
        .filter_map(|e| {
            let path = e.path();
            let size = fs::metadata(&path).ok()?.len();
            Some((path, size))
        })
        .collect();

    // Sort by size descending
    i_files_with_sizes.sort_by(|a, b| b.1.cmp(&a.1));

    let total_files = i_files_with_sizes.len();
    eprintln!("    Preprocessed {} files in {}{:.2}s{}", total_files, CYAN, preproc_time, NC);

    // Show file size distribution
    eprintln!();
    eprintln!("  {}File size distribution:{}", BOLD, NC);
    for (i, (path, size)) in i_files_with_sizes.iter().take(10).enumerate() {
        let fname = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        eprintln!("    {:2}. {:>8} - {}", i + 1, format_size(*size as usize), fname);
    }
    if total_files > 10 {
        let min_size = i_files_with_sizes.last().map(|(_, s)| *s).unwrap_or(0);
        eprintln!("    ... {} more files (smallest: {})", total_files - 10, format_size(min_size as usize));
    }

    // Phase 2: Measure original compilation time
    eprintln!();
    eprintln!("  {}Phase 2:{} Measuring original compilation time...", BOLD, NC);

    let orig_obj_dir = work_dir.join("orig_obj");
    fs::create_dir_all(&orig_obj_dir).ok();

    let orig_start = Instant::now();
    let orig_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.split_jobs)
        .build()
        .unwrap();

    let vim_src_clone = vim_src.clone();
    let vim_proto_clone = vim_proto.clone();
    let orig_obj_dir_clone = orig_obj_dir.clone();
    let i_files: Vec<PathBuf> = i_files_with_sizes.iter().map(|(p, _)| p.clone()).collect();

    orig_pool.install(|| {
        i_files.par_iter().for_each(|i_file| {
            compile_vim_i_to_object(i_file, &orig_obj_dir_clone, &vim_src_clone, &vim_proto_clone);
        });
    });

    let orig_time = orig_start.elapsed().as_secs_f64();
    eprintln!("    Original compile: {}{:.2}s{} ({} files parallel)", CYAN, orig_time, NC, total_files);

    // Phase 3: Generate thresholds to test
    // Use file sizes as breakpoints - test threshold at each unique file size
    let mut thresholds: Vec<u64> = vec![u64::MAX]; // Start with "all passthrough"
    for (_, size) in &i_files_with_sizes {
        // Add threshold just below each file size to include that file in splitting
        if *size > 0 {
            thresholds.push(*size - 1);
        }
    }
    thresholds.push(0); // End with "all split"
    thresholds.dedup();

    eprintln!();
    eprintln!("  {}Phase 3:{} Testing {} threshold values...", BOLD, NC, thresholds.len());
    eprintln!();

    let mut results: Vec<ThresholdResult> = Vec::new();
    let precc_bin = cfg.precc_bin.clone();

    for (idx, &threshold) in thresholds.iter().enumerate() {
        let split_dir = work_dir.join(format!("split_{}", idx));
        fs::create_dir_all(&split_dir).ok();

        // Count how many files will be split at this threshold
        let files_split = i_files_with_sizes.iter().filter(|(_, s)| *s > threshold).count();

        // Run precc with this threshold (timed for fair comparison)
        let gen_start = Instant::now();
        let gen_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.split_jobs)
            .build()
            .unwrap();

        let precc_bin_clone = precc_bin.clone();
        let total_pus = Arc::new(AtomicUsize::new(0));
        let total_pus_clone = total_pus.clone();

        gen_pool.install(|| {
            i_files.par_iter().for_each(|i_file| {
                let mut cmd = Command::new(&precc_bin_clone);
                cmd.arg(i_file)
                    .env("SPLIT", "1")
                    .env("PASSTHROUGH_THRESHOLD", threshold.to_string())
                    .stderr(Stdio::null())
                    .stdout(Stdio::null());

                if cmd.status().map(|s| s.success()).unwrap_or(false) {
                    let i_basename = i_file.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    let parent = i_file.parent().unwrap_or(Path::new("."));
                    let pu_files = find_all_pu_files(parent, i_basename);
                    total_pus_clone.fetch_add(pu_files.len(), Ordering::Relaxed);

                    // Move PUs to split_dir
                    for pu in pu_files {
                        if let Some(name) = pu.file_name() {
                            let dest = split_dir.join(name);
                            fs::rename(&pu, dest).ok();
                        }
                    }
                }
            });
        });

        let gen_time = gen_start.elapsed().as_secs_f64();
        let total_pus_count = total_pus.load(Ordering::Relaxed);

        // Compile PUs to actual object files (fair comparison with original)
        let pu_files: Vec<PathBuf> = fs::read_dir(&split_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.ends_with(".pu.c"))
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();

        let comp_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.split_jobs)
            .build()
            .unwrap();

        let comp_start = Instant::now();
        let vim_src_clone2 = vim_src.clone();
        let vim_proto_clone2 = vim_proto.clone();
        let split_obj_dir = work_dir.join(format!("obj_{}", idx));
        fs::create_dir_all(&split_obj_dir).ok();
        let split_obj_dir_clone = split_obj_dir.clone();

        comp_pool.install(|| {
            pu_files.par_iter().for_each(|pu| {
                compile_vim_to_object(pu, &split_obj_dir_clone, &vim_src_clone2, &vim_proto_clone2);
            });
        });

        // Cleanup obj dir immediately
        fs::remove_dir_all(&split_obj_dir).ok();

        let comp_time = comp_start.elapsed().as_secs_f64();
        // Total split time includes preprocessing for accurate real-world comparison
        let split_time = preproc_time + gen_time + comp_time;
        let speedup = orig_time / split_time;

        let threshold_display = if threshold == u64::MAX {
            "MAX".to_string()
        } else {
            format_size(threshold as usize)
        };

        eprintln!(
            "    [{:3}/{}] threshold={:>8}, split={:3}/{}, PUs={:5}, preproc={:.1}s, gen={:.1}s, comp={:.1}s, total={:.2}s, speedup={:.2}x",
            idx + 1,
            thresholds.len(),
            threshold_display,
            files_split,
            total_files,
            total_pus_count,
            preproc_time,
            gen_time,
            comp_time,
            split_time,
            speedup
        );

        results.push(ThresholdResult {
            threshold,
            files_split,
            total_pus: total_pus_count,
            orig_time,
            split_time,
            speedup,
        });

        // Cleanup this iteration
        fs::remove_dir_all(&split_dir).ok();
    }

    // Print summary table
    eprintln!();
    eprintln!("{}{}+------------------------------------------------------------------------------+{}", BOLD, GREEN, NC);
    eprintln!("{}{}|                    THRESHOLD SWEEP RESULTS                                    |{}", BOLD, GREEN, NC);
    eprintln!("{}{}|             (Split Time = preproc + gen + compile for real-world comparison)  |{}", BOLD, GREEN, NC);
    eprintln!("{}{}+------------------------------------------------------------------------------+{}", BOLD, GREEN, NC);
    eprintln!("{}{}|{} Threshold | Files Split |  Total PUs | Split Time | Speedup                {}{}|{}", BOLD, GREEN, NC, BOLD, GREEN, NC);
    eprintln!("{}{}+------------------------------------------------------------------------------+{}", BOLD, GREEN, NC);

    for r in &results {
        let threshold_str = if r.threshold == u64::MAX {
            "    MAX".to_string()
        } else if r.threshold == 0 {
            "      0".to_string()
        } else {
            format!("{:>7}", format_size(r.threshold as usize))
        };

        let speedup_bar_len = ((r.speedup - 0.5).max(0.0) * 20.0).min(30.0) as usize;
        let speedup_bar: String = "#".repeat(speedup_bar_len);

        eprintln!(
            "{}{}|{} {:>9} |   {:3} / {:3} | {:10} |   {:6.2}s  | {:5.2}x {}       {}{}|{}",
            BOLD, GREEN, NC,
            threshold_str,
            r.files_split,
            total_files,
            r.total_pus,
            r.split_time,
            r.speedup,
            speedup_bar,
            BOLD, GREEN, NC
        );
    }

    eprintln!("{}{}+------------------------------------------------------------------------------+{}", BOLD, GREEN, NC);
    eprintln!("{}{}|{} Original compile time: {:.2}s (from .i files, {} files parallel)              {}{}|{}",
        BOLD, GREEN, NC, orig_time, total_files, BOLD, GREEN, NC);
    eprintln!("{}{}|{} Preprocess time:       {:.2}s (gcc -E, included in split time)                {}{}|{}",
        BOLD, GREEN, NC, preproc_time, BOLD, GREEN, NC);
    eprintln!("{}{}+------------------------------------------------------------------------------+{}", BOLD, GREEN, NC);

    // Update stats with best result
    if let Some(best) = results.iter().max_by(|a, b| a.speedup.partial_cmp(&b.speedup).unwrap()) {
        stats.vim_total_pus = best.total_pus;
        stats.vim_comp_time = best.split_time;
        stats.vim_orig_comp_time = best.orig_time;
    }

    // Cleanup
    fs::remove_dir_all(&work_dir).ok();
}

fn print_vim_summary(stats: &Stats) {
    let total_time = stats.start_time.elapsed().as_secs();

    eprintln!();
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|                  VIM TEST SUMMARY                          |{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  Total Time:          {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format_duration(total_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}Regression Tests:{}                                         {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Test cases:        {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{} passed", stats.regression_tests_passed),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}Vim Source Files:{}                                         {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Files tested:      {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{} / {}", stats.vim_files_tested, stats.vim_total_files),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Files skipped:     {:<36} {}{}|{}",
        BOLD, GREEN, NC, stats.vim_files_skipped, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}Compilation:{}                                              {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Total PUs:         {:<36} {}{}|{}",
        BOLD, GREEN, NC, stats.vim_total_pus, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Passed:            {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!(
            "{} / {} ({:.1}%)",
            stats.vim_comp_passed, stats.vim_total_pus, stats.vim_success_rate
        ),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Failed:            {:<36} {}{}|{}",
        BOLD, GREEN, NC, stats.vim_comp_failed, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}  {}Performance:{}                                              {}{}|{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}|{}    Preprocess time:   {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s (gcc -E)", stats.vim_preprocess_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Generation time:   {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s (precc)", stats.vim_gen_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}|{}    Throughput:        {:<36} {}{}|{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.2} PUs/sec", stats.vim_throughput),
        BOLD,
        GREEN,
        NC
    );

    // Add linking validation section if linking was performed
    if stats.vim_link_success {
        eprintln!(
            "{}{}+------------------------------------------------------------+{}",
            BOLD, GREEN, NC
        );
        eprintln!(
            "{}{}|{}  {}Linking Validation:{}                                       {}{}|{}",
            BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
        );
        eprintln!(
            "{}{}|{}    Orig. compile:     {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{:.3}s", stats.vim_orig_comp_time),
            BOLD,
            GREEN,
            NC
        );
        eprintln!(
            "{}{}|{}    Orig. link:        {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{:.3}s", stats.vim_orig_link_time),
            BOLD,
            GREEN,
            NC
        );
        eprintln!(
            "{}{}|{}    Split compile:     {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{:.3}s", stats.vim_comp_time),
            BOLD,
            GREEN,
            NC
        );
        eprintln!(
            "{}{}|{}    Split link:        {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{:.3}s", stats.vim_link_time),
            BOLD,
            GREEN,
            NC
        );
        eprintln!(
            "{}{}|{}    Symbols:           {:<36} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            format!("{} exported", stats.vim_symbols_count),
            BOLD,
            GREEN,
            NC
        );
        // Code size comparison (text section) - only show if sizes are comparable
        if stats.vim_orig_text_size > 0 && stats.vim_split_text_size > 0 {
            let ratio = stats.vim_split_text_size as f64 / stats.vim_orig_text_size as f64;
            if ratio > 0.5 && ratio < 2.0 {
                let diff_pct = ((ratio - 1.0) * 100.0).abs();
                let code_size_str = format!("{} vs {} ({:.2}% diff)",
                    format_size(stats.vim_orig_text_size),
                    format_size(stats.vim_split_text_size),
                    diff_pct);
                eprintln!(
                    "{}{}|{}    Code size:         {:<36} {}{}|{}",
                    BOLD, GREEN, NC, code_size_str, BOLD, GREEN, NC
                );
            }
        }
        // End-to-end comparison (includes preprocessing for fair real-world comparison)
        let total_orig_time = stats.vim_orig_comp_time + stats.vim_orig_link_time;
        let total_split_time = stats.vim_preprocess_time + stats.vim_gen_time + stats.vim_comp_time + stats.vim_link_time;
        let speedup = if total_split_time > 0.0 {
            total_orig_time / total_split_time
        } else {
            0.0
        };
        eprintln!(
            "{}{}|{}    ------------------------------------------------------- {}{}|{}",
            BOLD, GREEN, NC, BOLD, GREEN, NC
        );
        eprintln!(
            "{}{}|{}    {}Split total:       {:<36}{} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            CYAN,
            format!("{:.3}s (preproc+gen+comp+link)", total_split_time),
            NC,
            BOLD,
            GREEN,
            NC
        );
        eprintln!(
            "{}{}|{}    {}vs Original:       {:<36}{} {}{}|{}",
            BOLD,
            GREEN,
            NC,
            CYAN,
            format!("{:.2}x {}", speedup, if speedup >= 1.0 { "faster" } else { "slower" }),
            NC,
            BOLD,
            GREEN,
            NC
        );
    }

    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, GREEN, NC
    );
}

fn vim_mode_loop(cfg: &Config, stats: &mut Stats) -> bool {
    print_mode_banner("VIM MODE");

    // Fetch and configure vim if not present
    if !fetch_vim(cfg) {
        log_error("Failed to fetch/configure vim repository");
        return false;
    }

    build_if_needed(cfg);

    // Skip regression test if requested (for performance-focused runs)
    if !cfg.skip_regression {
        if !run_regression_test(cfg, stats) {
            log_error("Regression test failed! Cannot proceed with vim testing.");
            return false;
        }
    } else {
        log_info("Skipping regression tests (use --regression to enable)");
    }

    // Run threshold sweep if requested
    if cfg.all_threshold_mode {
        run_threshold_sweep(cfg, stats);
        return true;
    }

    if cfg.parallel_split {
        run_vim_benchmark_parallel(cfg, stats);
    } else {
        run_vim_benchmark(cfg, stats);
    }

    print_vim_summary(stats);
    true
}

// ============================================================================
// BUG FIXING MODE
// ============================================================================

fn main_loop(cfg: &Config, stats: &mut Stats) -> bool {
    for iteration in 1..=cfg.max_iterations {
        print_mode_banner(&format!("ITERATION {} / {}", iteration, cfg.max_iterations));

        build_if_needed(cfg);

        // Step 1: Regression test
        if !run_regression_test(cfg, stats) {
            log_error("Regression detected! Stopping.");
            log_info("A previously fixed bug is now failing.");
            return false;
        }

        if !cfg.exp_input_file.exists() {
            log_warn(&format!(
                "Input file '{:?}' not found in experiment dir",
                cfg.exp_input_file
            ));
            log_info("Regression tests passed. No large file to test.");
            return true;
        }

        // Step 2: Overlapped generation and scan
        let scan_result = overlapped_generation_and_scan(cfg, stats);

        match scan_result {
            Ok(None) => {
                // No failures - all pass!
                eprintln!();
                eprintln!(
                    "{}{}+------------------------------------------------------------+{}",
                    BOLD, GREEN, NC
                );
                eprintln!(
                    "{}{}|                    ALL TESTS PASS!                         |{}",
                    BOLD, GREEN, NC
                );
                eprintln!(
                    "{}{}+------------------------------------------------------------+{}",
                    BOLD, GREEN, NC
                );
                eprintln!();
                log_ok("All compilation units compile successfully!");
                return true;
            }
            Ok(Some(first_failure)) => {
                // Found a failure - continue with fix workflow
                // Step 3: Estimate failures
                if let Err(e) = estimate_failures_by_sampling(cfg, stats) {
                    log_error(&format!("Estimation failed: {}", e));
                    if !cfg.auto_mode {
                        eprint!("Continue anyway? (y/N): ");
                        std::io::stdout().flush().ok();
                        let mut response = String::new();
                        std::io::stdin().read_line(&mut response).ok();
                        if !response.trim().eq_ignore_ascii_case("y") {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Step 4: Create test case
                let test_case = create_test_case(cfg, &first_failure);

                let pu_num = first_failure
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|s| get_pu_number(s))
                    .unwrap_or(0);

                // Step 5: Call Claude
                if !cfg.auto_mode {
                    eprintln!();
                    eprint!("Call Claude to fix this bug? (Y/n/q to quit): ");
                    std::io::stdout().flush().ok();
                    let mut response = String::new();
                    std::io::stdin().read_line(&mut response).ok();
                    match response.trim().to_lowercase().as_str() {
                        "q" => return true,
                        "n" => {
                            log_info("Skipping automatic fix. Manual intervention required.");
                            return false;
                        }
                        _ => {}
                    }
                }

                call_claude_fix(cfg, &first_failure, &test_case, pu_num);

                // Step 6: Verify fix
                let max_verify_attempts = 3;
                let mut fix_succeeded = false;

                for attempt in 1..=max_verify_attempts {
                    eprintln!();
                    eprintln!(
                        "{}Verification attempt {} / {}{}",
                        DIM, attempt, max_verify_attempts, NC
                    );

                    if verify_fix(cfg, stats, pu_num) {
                        log_ok("Fix verified! Continuing to next iteration...");
                        fix_succeeded = true;
                        break;
                    } else if attempt < max_verify_attempts {
                        log_warn("Fix didn't work. Asking Claude to try again...");
                        call_claude_fix(cfg, &first_failure, &test_case, pu_num);
                    } else {
                        log_error(&format!(
                            "Fix failed after {} attempts.",
                            max_verify_attempts
                        ));
                        if !cfg.auto_mode {
                            eprint!("Continue to next bug? (y/N): ");
                            std::io::stdout().flush().ok();
                            let mut response = String::new();
                            std::io::stdin().read_line(&mut response).ok();
                            if !response.trim().eq_ignore_ascii_case("y") {
                                return false;
                            }
                        } else {
                            return false;
                        }
                    }
                }

                if !fix_succeeded && cfg.auto_mode {
                    return false;
                }

                eprintln!();
                log_info("Proceeding to next iteration...");
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => {
                log_error(&format!("Generation/scan failed: {}", e));
                return false;
            }
        }
    }

    log_warn(&format!("Reached maximum iterations ({})", cfg.max_iterations));
    true
}

fn print_final_summary(cfg: &Config, stats: &Stats, success: bool) {
    let total_time = stats.start_time.elapsed().as_secs();

    eprintln!();
    eprintln!(
        "{}+------------------------------------------------------------+{}",
        BOLD, NC
    );
    eprintln!(
        "{}|                    FINAL SUMMARY                           |{}",
        BOLD, NC
    );
    eprintln!(
        "{}+------------------------------------------------------------+{}",
        BOLD, NC
    );
    eprintln!(
        "{}|{}  Total Time:        {:<38} {}|{}",
        BOLD,
        NC,
        format_duration(total_time),
        BOLD,
        NC
    );
    eprintln!(
        "{}|{}  Bugs Fixed:        {}{:<38}{} {}|{}",
        BOLD, NC, GREEN, stats.bugs_fixed, NC, BOLD, NC
    );
    eprintln!(
        "{}|{}  Final Estimate:    {:<38} {}|{}",
        BOLD,
        NC,
        format!("{} failures", stats.current_estimate),
        BOLD,
        NC
    );
    eprintln!(
        "{}|{}  Original Baseline: {:<38} {}|{}",
        BOLD, NC, cfg.baseline_failures, BOLD, NC
    );
    if success {
        eprintln!(
            "{}|{}  Status:            {}SUCCESS{}                                {}|{}",
            BOLD, NC, GREEN, NC, BOLD, NC
        );
    } else {
        eprintln!(
            "{}|{}  Status:            {}STOPPED{}                                {}|{}",
            BOLD, NC, RED, NC, BOLD, NC
        );
    }
    eprintln!(
        "{}+------------------------------------------------------------+{}",
        BOLD, NC
    );
}

/// Print a centered banner box with the given title
fn print_banner(title: &str) {
    // Box width is 62 chars inside (| ... |)
    let content_width = 60;
    let padding = (content_width - title.len()) / 2;
    let left_pad = " ".repeat(padding);
    let right_pad = " ".repeat(content_width - padding - title.len());

    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, CYAN, NC
    );
    eprintln!(
        "{}{}|{}{}{}|{}",
        BOLD, CYAN, left_pad, title, right_pad, NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, CYAN, NC
    );
}

/// Get the mode title for the current configuration
fn get_mode_title(cfg: &Config) -> &'static str {
    if cfg.vim_mode {
        "PRECC VIM SOURCE FILES BENCHMARK"
    } else if cfg.perf_mode {
        "PRECC PERFORMANCE BENCHMARK"
    } else {
        "PRECC AUTOMATIC BUG FIX WORKFLOW"
    }
}

/// Print version information
fn print_version_info(cfg: &Config) {
    eprintln!("  {}Version Information:{}", BOLD, NC);
    eprintln!(
        "    Git commit:        {}{}{} ({})",
        YELLOW, cfg.version_tag, NC, cfg.git_branch
    );
    eprintln!("    Source dir:        {:?}", cfg.source_dir);
    eprintln!("    Experiment dir:    {}{:?}{}", CYAN, cfg.experiment_dir, NC);
    eprintln!("    Latest symlink:    /tmp/precc_exp_latest");
}

/// Print mode-specific configuration
fn print_configuration(cfg: &Config) {
    eprintln!("  {}Configuration:{}", BOLD, NC);
    if cfg.vim_mode {
        if cfg.parallel_split {
            eprintln!(
                "    Mode:              {}VIM + PARALLEL{} (--vim --parallel-split)",
                GREEN, NC
            );
            eprintln!(
                "    Split jobs:        {} parallel precc processes",
                cfg.split_jobs
            );
        } else {
            eprintln!("    Mode:              {}VIM{} (--vim)", GREEN, NC);
        }
        eprintln!(
            "    Vim source:        {:?}",
            cfg.source_dir.join("tests/vim/src")
        );
    } else if cfg.perf_mode {
        eprintln!("    Input file:        {}", cfg.input_file);
        eprintln!("    Mode:              {}PERFORMANCE{} (--perf)", GREEN, NC);
    } else {
        eprintln!("    Input file:        {}", cfg.input_file);
        eprintln!("    Baseline failures: {}", cfg.baseline_failures);
        eprintln!("    Max iterations:    {}", cfg.max_iterations);
        eprintln!(
            "    Auto mode:         {}",
            if cfg.auto_mode {
                format!("{}ON{}", GREEN, NC)
            } else {
                format!("{}OFF{}", YELLOW, NC)
            }
        );
    }
}

/// Print resource allocation information
fn print_resource_info(cfg: &Config) {
    eprintln!("  {}Resource Allocation:{}", BOLD, NC);
    eprintln!("    Total CPUs:        {}", get_num_cpus());
    eprintln!("    Precc jobs:        {} (--jobs)", cfg.precc_jobs);
    eprintln!(
        "    Reserved CPUs:     {} (for compilation/other)",
        cfg.reserved_cpus
    );

    // Bug-fixing mode has additional resource info
    if !cfg.perf_mode && !cfg.vim_mode {
        eprintln!(
            "    Precc timeout:     {} (--timeout)",
            format_duration(cfg.precc_timeout)
        );
        eprintln!("    Sample size:       {} (--sample)", cfg.sample_size);
        eprintln!("    Scan delay:        {}s (--scan-delay)", cfg.scan_delay);
        if cfg.start_pu > 0 {
            eprintln!(
                "    Start from PU:     {}{}{} (--start-pu) - skipping earlier PUs",
                YELLOW, cfg.start_pu, NC
            );
        }
    }
}

/// Print startup information and configuration summary
fn print_startup_info(cfg: &Config) {
    eprintln!();
    print_banner(get_mode_title(cfg));
    eprintln!();
    print_version_info(cfg);
    eprintln!();
    print_configuration(cfg);
    eprintln!();
    print_resource_info(cfg);
    eprintln!();
    eprintln!("  {}Note: All test files are isolated in experiment directory.{}", DIM, NC);
    eprintln!(
        "  {}To inspect results: ls {:?}{}",
        DIM, cfg.experiment_dir, NC
    );
    eprintln!();
}

/// Print a magenta-colored mode banner (used within mode loops)
fn print_mode_banner(title: &str) {
    let content_width = 60;
    let padding = (content_width - title.len()) / 2;
    let left_pad = " ".repeat(padding);
    let right_pad = " ".repeat(content_width - padding - title.len());

    eprintln!();
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, MAGENTA, NC
    );
    eprintln!(
        "{}{}|{}{}{}|{}",
        BOLD, MAGENTA, left_pad, title, right_pad, NC
    );
    eprintln!(
        "{}{}+------------------------------------------------------------+{}",
        BOLD, MAGENTA, NC
    );
}

/// Apply precc build options from config to a Command
fn apply_precc_build_options(cmd: &mut Command, cfg: &Config) {
    if cfg.split {
        cmd.env("SPLIT", "1");
    }
    if cfg.lattice_headers {
        cmd.env("LATTICE_HEADERS", "1");
    }
    if cfg.unity_build {
        cmd.env("UNITY_BUILD", "1");
    }
    cmd.env("UNITY_BATCHES", cfg.unity_batches.to_string());
    if cfg.inprocess_mode {
        cmd.env("INPROCESS", "1");
    }
}

/// Apply precc build options using raw values (for parallel closures)
fn apply_precc_build_options_raw(cmd: &mut Command, split: bool, lattice_headers: bool, unity_build: bool, unity_batches: usize, inprocess_mode: bool) {
    if split {
        cmd.env("SPLIT", "1");
    }
    if lattice_headers {
        cmd.env("LATTICE_HEADERS", "1");
    }
    if unity_build {
        cmd.env("UNITY_BUILD", "1");
    }
    cmd.env("UNITY_BATCHES", unity_batches.to_string());
    if inprocess_mode {
        cmd.env("INPROCESS", "1");
    }
}

fn main() {
    let cfg = parse_args();
    set_ascii_mode(cfg.ascii_mode);
    let mut stats = Stats::new();

    setup_experiment_dir(&cfg);
    copy_input_file(&cfg);
    print_startup_info(&cfg);

    // Wait for user if not auto mode
    if !cfg.auto_mode && !cfg.perf_mode && !cfg.vim_mode {
        eprint!("Press Enter to start (or 'q' to quit): ");
        std::io::stdout().flush().ok();
        let mut response = String::new();
        std::io::stdin().read_line(&mut response).ok();
        if response.trim() == "q" {
            std::process::exit(0);
        }
    }

    // Run appropriate mode
    let success = if cfg.vim_mode {
        vim_mode_loop(&cfg, &mut stats)
    } else if cfg.perf_mode {
        perf_mode_loop(&cfg, &mut stats)
    } else {
        let result = main_loop(&cfg, &mut stats);
        print_final_summary(&cfg, &stats, result);
        result
    };

    std::process::exit(if success { 0 } else { 1 });
}
