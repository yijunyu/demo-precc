//! test_batch - Iterative batch test script with automatic bug fixing via Claude
//!
//! Modes:
//! - Bug Fixing (default): Iterative bug detection and Claude-assisted fixing
//! - Performance (--perf): Benchmark generation and compilation throughput
//! - Vim (--vim): Test all vim source files

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
const FIXED_BUGS: &[&str] = &[
    "bug1", "bug2", "bug4", "bug5", "bug6", "bug7", "bug8", "bug9", "bug12", "bug13", "bug14",
    "bug15", "bug16", "bug17", "bug18", "bug19", "bug20", "bug21", "bug22", "bug23", "bug24",
    "bug25", "bug26", "bug28", "bug29", "bug30", "bug32", "bug33", "bug34", "bug35", "bug36",
    "bug37", "bug38", "bug39", "bug40", "bug41", "bug42", "bug43", "bug44", "bug45", "bug46",
    "bug47", "bug48", "bug49", "bug50", "bug51", "bug52", "bug53", "bug54", "bug55", "bug56",
    "bug57", "bug58", "bug59", "bug60", "bug61", "bug62", "bug65", "bug66", "bug68", "bug69",
    "bug70", "bug71",
];

#[derive(Clone)]
struct Config {
    auto_mode: bool,
    perf_mode: bool,
    vim_mode: bool,
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
}

struct Stats {
    total_pus: usize,
    bugs_fixed: usize,
    current_estimate: usize,
    start_time: Instant,
    regression_tests_passed: usize,
    // Perf mode stats
    perf_gen_time: f64,
    perf_gen_throughput: f64,
    perf_comp_time: f64,
    perf_comp_throughput: f64,
    perf_comp_passed: usize,
    perf_comp_failed: usize,
    perf_success_rate: f64,
    // Vim mode stats
    vim_total_files: usize,
    vim_files_tested: usize,
    vim_files_skipped: usize,
    vim_total_pus: usize,
    vim_comp_passed: usize,
    vim_comp_failed: usize,
    vim_gen_time: f64,
    vim_success_rate: f64,
    vim_throughput: f64,
}

impl Stats {
    fn new() -> Self {
        Stats {
            total_pus: 0,
            bugs_fixed: 0,
            current_estimate: 0,
            start_time: Instant::now(),
            regression_tests_passed: 0,
            perf_gen_time: 0.0,
            perf_gen_throughput: 0.0,
            perf_comp_time: 0.0,
            perf_comp_throughput: 0.0,
            perf_comp_passed: 0,
            perf_comp_failed: 0,
            perf_success_rate: 0.0,
            vim_total_files: 0,
            vim_files_tested: 0,
            vim_files_skipped: 0,
            vim_total_pus: 0,
            vim_comp_passed: 0,
            vim_comp_failed: 0,
            vim_gen_time: 0.0,
            vim_success_rate: 0.0,
            vim_throughput: 0.0,
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
    eprintln!("{}{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}", BOLD, CYAN, NC);
    eprintln!("{}{}  STAGE: {}{}", BOLD, CYAN, name, NC);
    eprintln!("{}{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}", BOLD, CYAN, NC);
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
        r#"PRECC Batch Test Script - Automatic Bug Detection and Fixing

USAGE:
    test_batch [input_file.i] [baseline_failures] [options]

ARGUMENTS:
    input_file.i        Preprocessed C file to test (default: sqlite3.i)
    baseline_failures   Expected number of failures (default: 310)

OPTIONS:
    --help, -h          Show this help message and exit
    --auto              Run fully automatically without confirmation prompts
    --perf              Performance mode: skip bug fixing, focus on benchmarking
    --vim               Vim mode: test all vim source files in tests/vim/src
    --parallel-split    Parallelize precc split commands (with --vim)
    --inprocess         Use thread-safe in-process parallel ctags
    --split-jobs N      Number of parallel precc jobs (default: --jobs value)
    --max N             Maximum number of fix iterations (default: 10)
    --jobs N            Parallel jobs for precc generation (default: half of CPUs)
    --timeout N         Timeout for precc generation in seconds (default: 1800)
    --sample N          Sample size for failure estimation (default: 150)
    --scan-delay N      Delay before starting scan in seconds (default: 5)
    --exp-dir DIR       Use specific experiment directory
    --start-pu N        Start from PU N, skip generating/scanning earlier PUs

MODES:
    Bug Fixing (default):  Iterative bug detection and Claude-assisted fixing
    Performance (--perf):  Benchmark generation and compilation throughput
    Vim (--vim):           Test all vim source files
"#
    );
}

fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();

    let mut auto_mode = false;
    let mut perf_mode = false;
    let mut vim_mode = false;
    let mut parallel_split = false;
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

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--auto" => auto_mode = true,
            "--perf" => perf_mode = true,
            "--vim" => vim_mode = true,
            "--parallel-split" => parallel_split = true,
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
    if release_bin.exists() {
        fs::copy(&release_bin, &cfg.precc_bin).ok();
    }
}

fn copy_input_file(cfg: &Config) {
    let input_path = PathBuf::from(&cfg.input_file);
    if input_path.exists() {
        fs::copy(&input_path, &cfg.exp_input_file).ok();
    }
}

fn build_if_needed(cfg: &Config) {
    let release_bin = cfg.source_dir.join("target/release/precc");
    let lib_rs = cfg.source_dir.join("src/lib.rs");

    let needs_build = !release_bin.exists()
        || lib_rs
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .zip(release_bin.metadata().and_then(|m| m.modified()).ok())
            .map(|(src, bin)| src > bin)
            .unwrap_or(true);

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
    Command::new("gcc")
        .args(["-c"])
        .arg(path)
        .arg("-I")
        .arg(vim_src)
        .arg("-I")
        .arg(vim_proto)
        .args(["-DHAVE_CONFIG_H", "-o", "/dev/null"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn find_pu_files(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(pattern) && name.ends_with(".pu.c") {
                    files.push(path);
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

        let i_file = cfg.tests_dir.join(format!("{}.i", bug));

        // Cleanup
        fs::remove_file(&i_file).ok();
        for entry in fs::read_dir(&cfg.tests_dir).into_iter().flatten() {
            if let Ok(entry) = entry {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&format!("{}.i_", bug)) && name_str.ends_with(".pu.c") {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }

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

        // Run precc
        let precc_status = Command::new(&cfg.precc_bin)
            .arg(&i_file)
            .env("PASSTHROUGH_THRESHOLD", "0")
            .env("SPLIT", "1")
            .env("JOBS", "1")
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status();

        if !precc_status.map(|s| s.success()).unwrap_or(false) {
            continue;
        }

        // Compile PUs
        let mut ok = true;
        for entry in fs::read_dir(&cfg.tests_dir).into_iter().flatten() {
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

        if ok {
            passed += 1;
        } else {
            regressions += 1;
            eprintln!();
            log_error(&format!("REGRESSION: {}", bug));
        }
    }

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
        .env("SPLIT", "1")
        .env("JOBS", cfg.precc_jobs.to_string())
        .stderr(Stdio::null())
        .stdout(Stdio::null());

    if cfg.inprocess_mode {
        cmd.env("INPROCESS", "1");
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

    let spinchars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
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
        let c = spinchars[spin_i % spinchars.len()];
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
                 // Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug{}.i\n\n\
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
   - `PU_FILTER={} PASSTHROUGH_THRESHOLD=0 SPLIT=1 target/release/precc {}`
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
    let regen_status = Command::new(&cfg.precc_bin)
        .arg(&cfg.exp_input_file)
        .current_dir(&cfg.experiment_dir)
        .env("PU_FILTER", pu_num.to_string())
        .env("PASSTHROUGH_THRESHOLD", "0")
        .env("SPLIT", "1")
        .env("JOBS", "1")
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status();

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
            "  {}✓ Bug fixed!{} Total bugs fixed this session: {}{}{}",
            GREEN, NC, BOLD, stats.bugs_fixed, NC
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

    let mut cmd = Command::new(&cfg.precc_bin);
    cmd.arg(&cfg.exp_input_file)
        .current_dir(&cfg.experiment_dir)
        .env("PASSTHROUGH_THRESHOLD", "0")
        .env("SPLIT", "1")
        .env("JOBS", cfg.precc_jobs.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if cfg.inprocess_mode {
        cmd.env("INPROCESS", "1");
    }

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
    let pattern = format!("{}_", input_basename);
    let pu_count = find_pu_files(&cfg.experiment_dir, &pattern).len();
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
    eprintln!(
        "{}Compiling all PUs with parallel gcc...{}",
        DIM, NC
    );
    eprintln!();

    let input_basename = cfg
        .exp_input_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    let pattern = format!("{}_", input_basename);
    let pu_files = find_pu_files(&cfg.experiment_dir, &pattern);
    let total = pu_files.len();

    eprintln!("  {}Configuration:{}", BOLD, NC);
    eprintln!("    Total PUs:      {}", total);
    eprintln!("    Parallel jobs:  {} (gcc instances)", cfg.reserved_cpus);
    eprintln!();

    let comp_start = Instant::now();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.reserved_cpus)
        .build()
        .unwrap();

    let passed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));

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

    let comp_time = comp_start.elapsed().as_secs_f64();
    let passed_count = passed.load(Ordering::Relaxed);
    let failed_count = failed.load(Ordering::Relaxed);

    let success_rate = passed_count as f64 * 100.0 / total as f64;
    let throughput = total as f64 / comp_time;

    eprintln!();
    eprintln!("  {}Compilation Results:{}", BOLD, NC);
    eprintln!(
        "    Passed:         {}{}{} / {} ({:.1}%)",
        GREEN, passed_count, NC, total, success_rate
    );
    eprintln!("    Failed:         {}{}{}", RED, failed_count, NC);
    eprintln!("    Total time:     {}{:.3}s{}", CYAN, comp_time, NC);
    eprintln!("    Throughput:     {}{:.2} PUs/sec{}", CYAN, throughput, NC);
    eprintln!();

    stats.perf_comp_time = comp_time;
    stats.perf_comp_throughput = throughput;
    stats.perf_comp_passed = passed_count;
    stats.perf_comp_failed = failed_count;
    stats.perf_success_rate = success_rate;

    true
}

fn print_perf_summary(_cfg: &Config, stats: &Stats) {
    let total_time = stats.start_time.elapsed().as_secs();

    eprintln!();
    eprintln!(
        "{}{}╔════════════════════════════════════════════════════════════╗{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║              PERFORMANCE SUMMARY                           ║{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  Total Time:          {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format_duration(total_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  {}Regression Tests:{}                                        {}{}║{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Test cases:        {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{} passed", stats.regression_tests_passed),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  {}Generation:{}                                              {}{}║{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    PUs generated:     {:<36} {}{}║{}",
        BOLD, GREEN, NC, stats.total_pus, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Time:              {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s", stats.perf_gen_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}║{}    Throughput:        {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.2} PUs/sec", stats.perf_gen_throughput),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  {}Compilation:{}                                             {}{}║{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Passed:            {:<36} {}{}║{}",
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
        "{}{}║{}    Failed:            {:<36} {}{}║{}",
        BOLD, GREEN, NC, stats.perf_comp_failed, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Time:              {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s", stats.perf_comp_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}║{}    Throughput:        {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.2} PUs/sec", stats.perf_comp_throughput),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}╚════════════════════════════════════════════════════════════╝{}",
        BOLD, GREEN, NC
    );
}

fn perf_mode_loop(cfg: &Config, stats: &mut Stats) -> bool {
    eprintln!();
    eprintln!(
        "{}{}╔════════════════════════════════════════════════════════════╗{}",
        BOLD, MAGENTA, NC
    );
    eprintln!(
        "{}{}║              PERFORMANCE MODE                              ║{}",
        BOLD, MAGENTA, NC
    );
    eprintln!(
        "{}{}╚════════════════════════════════════════════════════════════╝{}",
        BOLD, MAGENTA, NC
    );

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
        let prep_status = Command::new("gcc")
            .args(["-E", "-I"])
            .arg(&vim_src)
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
            .env("SPLIT", "1")
            .stderr(Stdio::null())
            .stdout(Stdio::null());

        if cfg.inprocess_mode {
            cmd.env("INPROCESS", "1");
        }

        if !cmd.status().map(|s| s.success()).unwrap_or(false) {
            stats.vim_files_skipped += 1;
            fs::remove_file(&i_file).ok();
            continue;
        }

        let file_gen_time = file_gen_start.elapsed().as_secs_f64();
        stats.vim_gen_time += file_gen_time;

        // Count and compile PUs
        let pattern = format!("{}.i_", fname);
        let pu_files = find_pu_files(&work_dir, &pattern);
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

    let prep_time = prep_start.elapsed().as_secs();
    let prep_count: usize = fs::read_dir(&prep_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.is_ok())
        .count();
    eprintln!(
        "    Preprocessed {}{}{} files in {}{}s{}",
        GREEN, prep_count, NC, CYAN, prep_time, NC
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

    gen_pool.install(|| {
        i_files.par_iter().for_each(|i_file| {
            let mut cmd = Command::new(&precc_bin);
            cmd.arg(i_file)
                .env("PASSTHROUGH_THRESHOLD", "0")
                .env("SPLIT", "1")
                .stderr(Stdio::null())
                .stdout(Stdio::null());

            if inprocess_mode {
                cmd.env("INPROCESS", "1");
            }

            if cmd.status().map(|s| s.success()).unwrap_or(false) {
                // Count generated PUs
                let fname = i_file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let pattern = format!("{}.i_", fname);
                let parent = i_file.parent().unwrap_or(Path::new("."));
                let pu_count = find_pu_files(parent, &pattern).len();

                if pu_count > 0 {
                    // Move PUs to split_dir
                    for pu in find_pu_files(parent, &pattern) {
                        if let Some(name) = pu.file_name() {
                            let dest = split_dir.join(name);
                            fs::rename(&pu, dest).ok();
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

    let comp_time = comp_start.elapsed().as_secs();
    eprintln!(
        "    Compiled {}{}{} OK, {}{}{} FAIL in {}{}s{}",
        GREEN, stats.vim_comp_passed, NC, RED, stats.vim_comp_failed, NC, CYAN, comp_time, NC
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
        "    Generation time:  {}{:.3}s{} (parallel)",
        CYAN, stats.vim_gen_time, NC
    );
    eprintln!(
        "    Gen throughput:   {}{:.2} PUs/sec{}",
        CYAN, stats.vim_throughput, NC
    );
    eprintln!("    Compile time:     {}{}s{} (parallel)", CYAN, comp_time, NC);
    eprintln!("    Total time:       {}{:.3}s{}", CYAN, total_time, NC);

    // Cleanup
    fs::remove_dir_all(&prep_dir).ok();
    fs::remove_dir_all(&split_dir).ok();
}

fn print_vim_summary(stats: &Stats) {
    let total_time = stats.start_time.elapsed().as_secs();

    eprintln!();
    eprintln!(
        "{}{}╔════════════════════════════════════════════════════════════╗{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║                  VIM TEST SUMMARY                          ║{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  Total Time:          {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format_duration(total_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  {}Regression Tests:{}                                        {}{}║{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Test cases:        {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{} passed", stats.regression_tests_passed),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  {}Vim Source Files:{}                                        {}{}║{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Files tested:      {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{} / {}", stats.vim_files_tested, stats.vim_total_files),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}║{}    Files skipped:     {:<36} {}{}║{}",
        BOLD, GREEN, NC, stats.vim_files_skipped, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  {}Compilation:{}                                             {}{}║{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Total PUs:         {:<36} {}{}║{}",
        BOLD, GREEN, NC, stats.vim_total_pus, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Passed:            {:<36} {}{}║{}",
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
        "{}{}║{}    Failed:            {:<36} {}{}║{}",
        BOLD, GREEN, NC, stats.vim_comp_failed, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}  {}Performance:{}                                             {}{}║{}",
        BOLD, GREEN, NC, BOLD, NC, BOLD, GREEN, NC
    );
    eprintln!(
        "{}{}║{}    Generation time:   {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.3}s", stats.vim_gen_time),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}║{}    Throughput:        {:<36} {}{}║{}",
        BOLD,
        GREEN,
        NC,
        format!("{:.2} PUs/sec", stats.vim_throughput),
        BOLD,
        GREEN,
        NC
    );
    eprintln!(
        "{}{}╚════════════════════════════════════════════════════════════╝{}",
        BOLD, GREEN, NC
    );
}

fn vim_mode_loop(cfg: &Config, stats: &mut Stats) -> bool {
    eprintln!();
    eprintln!(
        "{}{}╔════════════════════════════════════════════════════════════╗{}",
        BOLD, MAGENTA, NC
    );
    eprintln!(
        "{}{}║                    VIM MODE                                ║{}",
        BOLD, MAGENTA, NC
    );
    eprintln!(
        "{}{}╚════════════════════════════════════════════════════════════╝{}",
        BOLD, MAGENTA, NC
    );

    build_if_needed(cfg);

    if !run_regression_test(cfg, stats) {
        log_error("Regression test failed! Cannot proceed with vim testing.");
        return false;
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
        eprintln!();
        eprintln!(
            "{}{}╔════════════════════════════════════════════════════════════╗{}",
            BOLD, MAGENTA, NC
        );
        eprintln!(
            "{}{}║              ITERATION {} / {}                              ║{}",
            BOLD, MAGENTA, iteration, cfg.max_iterations, NC
        );
        eprintln!(
            "{}{}╚════════════════════════════════════════════════════════════╝{}",
            BOLD, MAGENTA, NC
        );

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
                    "{}{}╔════════════════════════════════════════════════════════════╗{}",
                    BOLD, GREEN, NC
                );
                eprintln!(
                    "{}{}║                    ALL TESTS PASS!                         ║{}",
                    BOLD, GREEN, NC
                );
                eprintln!(
                    "{}{}╚════════════════════════════════════════════════════════════╝{}",
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
        "{}╔════════════════════════════════════════════════════════════╗{}",
        BOLD, NC
    );
    eprintln!(
        "{}║                    FINAL SUMMARY                           ║{}",
        BOLD, NC
    );
    eprintln!(
        "{}╠════════════════════════════════════════════════════════════╣{}",
        BOLD, NC
    );
    eprintln!(
        "{}║{}  Total Time:        {:<38} {}║{}",
        BOLD,
        NC,
        format_duration(total_time),
        BOLD,
        NC
    );
    eprintln!(
        "{}║{}  Bugs Fixed:        {}{:<38}{} {}║{}",
        BOLD, NC, GREEN, stats.bugs_fixed, NC, BOLD, NC
    );
    eprintln!(
        "{}║{}  Final Estimate:    {:<38} {}║{}",
        BOLD,
        NC,
        format!("{} failures", stats.current_estimate),
        BOLD,
        NC
    );
    eprintln!(
        "{}║{}  Original Baseline: {:<38} {}║{}",
        BOLD, NC, cfg.baseline_failures, BOLD, NC
    );
    if success {
        eprintln!(
            "{}║{}  Status:            {}SUCCESS{}                                {}║{}",
            BOLD, NC, GREEN, NC, BOLD, NC
        );
    } else {
        eprintln!(
            "{}║{}  Status:            {}STOPPED{}                                {}║{}",
            BOLD, NC, RED, NC, BOLD, NC
        );
    }
    eprintln!(
        "{}╚════════════════════════════════════════════════════════════╝{}",
        BOLD, NC
    );
}

fn main() {
    let cfg = parse_args();
    let mut stats = Stats::new();

    setup_experiment_dir(&cfg);
    copy_input_file(&cfg);

    // Clear screen and print header
    print!("\x1b[2J\x1b[H");
    eprintln!();

    if cfg.vim_mode {
        eprintln!(
            "{}{}╔════════════════════════════════════════════════════════════╗{}",
            BOLD, CYAN, NC
        );
        eprintln!(
            "{}{}║          PRECC VIM SOURCE FILES BENCHMARK                  ║{}",
            BOLD, CYAN, NC
        );
        eprintln!(
            "{}{}╚════════════════════════════════════════════════════════════╝{}",
            BOLD, CYAN, NC
        );
    } else if cfg.perf_mode {
        eprintln!(
            "{}{}╔════════════════════════════════════════════════════════════╗{}",
            BOLD, CYAN, NC
        );
        eprintln!(
            "{}{}║          PRECC PERFORMANCE BENCHMARK                       ║{}",
            BOLD, CYAN, NC
        );
        eprintln!(
            "{}{}╚════════════════════════════════════════════════════════════╝{}",
            BOLD, CYAN, NC
        );
    } else {
        eprintln!(
            "{}{}╔════════════════════════════════════════════════════════════╗{}",
            BOLD, CYAN, NC
        );
        eprintln!(
            "{}{}║          PRECC AUTOMATIC BUG FIX WORKFLOW                  ║{}",
            BOLD, CYAN, NC
        );
        eprintln!(
            "{}{}╚════════════════════════════════════════════════════════════╝{}",
            BOLD, CYAN, NC
        );
    }

    eprintln!();
    eprintln!("  {}Version Information:{}", BOLD, NC);
    eprintln!(
        "    Git commit:        {}{}{} ({})",
        YELLOW, cfg.version_tag, NC, cfg.git_branch
    );
    eprintln!("    Source dir:        {:?}", cfg.source_dir);
    eprintln!("    Experiment dir:    {}{:?}{}", CYAN, cfg.experiment_dir, NC);
    eprintln!("    Latest symlink:    /tmp/precc_exp_latest");
    eprintln!();

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

    eprintln!();
    eprintln!("  {}Resource Allocation:{}", BOLD, NC);
    eprintln!("    Total CPUs:        {}", get_num_cpus());
    eprintln!("    Precc jobs:        {} (--jobs)", cfg.precc_jobs);
    eprintln!(
        "    Reserved CPUs:     {} (for compilation/other)",
        cfg.reserved_cpus
    );

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

    eprintln!();
    eprintln!("  {}Note: All test files are isolated in experiment directory.{}", DIM, NC);
    eprintln!(
        "  {}To inspect results: ls {:?}{}",
        DIM, cfg.experiment_dir, NC
    );
    eprintln!();

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
