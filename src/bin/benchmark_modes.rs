//! benchmark_modes - Benchmark precc modes and configurations on vim codebase
//!
//! Measures speedup ratios for:
//! - Passthrough mode (no splitting)
//! - Split mode with various job counts
//! - Size-based threshold splitting

use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;

// ANSI colors
const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[1;33m";
const BLUE: &str = "\x1b[0;34m";
const CYAN: &str = "\x1b[0;36m";
const NC: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

// Files to skip (platform-specific or include-only files)
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
    "xdiff",
    "xpm",
    "libvterm",
    "winclip",
    "regexp_bt",
    "regexp_nfa",
];

#[derive(Clone)]
struct Config {
    num_runs: usize,
    max_jobs: usize,
    half_jobs: usize,
    quarter_jobs: usize,
    keep_work: bool,
    results_file: PathBuf,
    work_dir: PathBuf,
    vim_src: PathBuf,
    precc_bin: PathBuf,
}

#[derive(Clone, Debug)]
struct BenchResult {
    mode: String,
    config: String,
    files: usize,
    pus: usize,
    gen_time: f64,
    compile_time: f64,
    total_time: f64,
    success_rate: f64,
}

impl BenchResult {
    fn to_csv(&self) -> String {
        format!(
            "{},{},{},{},{:.3},{:.3},{:.3},{:.2}",
            self.mode,
            self.config,
            self.files,
            self.pus,
            self.gen_time,
            self.compile_time,
            self.total_time,
            self.success_rate
        )
    }
}

fn log(msg: &str) {
    let now = chrono::Local::now();
    eprintln!("{}[{}]{} {}", CYAN, now.format("%H:%M:%S"), NC, msg);
}

fn log_result(msg: &str) {
    eprintln!("{}[RESULT]{} {}", GREEN, NC, msg);
}

fn get_num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn should_skip(filename: &str) -> bool {
    for pattern in SKIP_PATTERNS {
        if filename.contains(pattern) {
            return true;
        }
    }
    false
}

fn get_vim_files(vim_src: &Path) -> Vec<String> {
    let mut files = Vec::new();

    if let Ok(entries) = fs::read_dir(vim_src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "c").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !should_skip(stem) {
                        files.push(stem.to_string());
                    }
                }
            }
        }
    }

    files.sort();
    files
}

fn count_loc(path: &Path) -> usize {
    if let Ok(file) = File::open(path) {
        BufReader::new(file).lines().count()
    } else {
        0
    }
}

fn preprocess_files(cfg: &Config, dest_dir: &Path) -> usize {
    fs::create_dir_all(dest_dir).ok();
    log("Preprocessing vim source files...");

    let files = get_vim_files(&cfg.vim_src);
    let count = AtomicUsize::new(0);

    files.par_iter().for_each(|name| {
        let src = cfg.vim_src.join(format!("{}.c", name));
        let dest = dest_dir.join(format!("{}.i", name));

        if src.exists() {
            let status = Command::new("gcc")
                .args(["-E", "-I"])
                .arg(&cfg.vim_src)
                .arg("-I")
                .arg(cfg.vim_src.join("proto"))
                .args(["-DHAVE_CONFIG_H"])
                .arg(&src)
                .arg("-o")
                .arg(&dest)
                .stderr(Stdio::null())
                .status();

            if status.map(|s| s.success()).unwrap_or(false) {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    count.load(Ordering::Relaxed)
}

fn compile_file(path: &Path) -> bool {
    Command::new("gcc")
        .args(["-c", "-O2"])
        .arg(path)
        .args(["-o", "/dev/null"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn find_i_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "i").unwrap_or(false) {
                files.push(path);
            }
        }
    }
    files
}

fn find_pu_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".pu.c") {
                    files.push(path);
                }
            }
        }
    }
    files
}

fn run_baseline(_cfg: &Config, src_dir: &Path, jobs: usize, _run_id: usize) -> BenchResult {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .unwrap();

    let files = find_i_files(src_dir);
    let total = files.len();

    let start = Instant::now();

    let ok = pool.install(|| {
        files
            .par_iter()
            .filter(|f| compile_file(f))
            .count()
    });

    let elapsed = start.elapsed().as_secs_f64();
    let success_rate = if total > 0 {
        (ok as f64 * 100.0) / total as f64
    } else {
        0.0
    };

    BenchResult {
        mode: "baseline".to_string(),
        config: format!("j{}", jobs),
        files: total,
        pus: total,
        gen_time: 0.0,
        compile_time: elapsed,
        total_time: elapsed,
        success_rate,
    }
}

fn run_passthrough(cfg: &Config, src_dir: &Path, jobs: usize, run_id: &str) -> BenchResult {
    let dest_dir = cfg.work_dir.join(format!("passthrough_{}", run_id));
    fs::create_dir_all(&dest_dir).ok();

    // Copy .i files
    let i_files = find_i_files(src_dir);
    let files_count = i_files.len();

    for ifile in &i_files {
        if let Some(name) = ifile.file_name() {
            let dest = dest_dir.join(name);
            fs::copy(ifile, dest).ok();
        }
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .unwrap();

    // Generate .pu.c files (passthrough = no split)
    let start_gen = Instant::now();

    let dest_i_files = find_i_files(&dest_dir);
    pool.install(|| {
        dest_i_files.par_iter().for_each(|ifile| {
            Command::new(&cfg.precc_bin)
                .arg(ifile)
                .env("SPLIT", "0")
                .stderr(Stdio::null())
                .stdout(Stdio::null())
                .status()
                .ok();
        });
    });

    let gen_time = start_gen.elapsed().as_secs_f64();

    // Count generated PUs
    let pu_files = find_pu_files(&dest_dir);
    let pus = if pu_files.is_empty() {
        files_count
    } else {
        pu_files.len()
    };

    // Compile
    let start_compile = Instant::now();
    let ok = pool.install(|| {
        pu_files
            .par_iter()
            .filter(|f| compile_file(f))
            .count()
    });
    let compile_time = start_compile.elapsed().as_secs_f64();

    let total_time = gen_time + compile_time;
    let success_rate = if pus > 0 {
        (ok as f64 * 100.0) / pus as f64
    } else {
        0.0
    };

    fs::remove_dir_all(&dest_dir).ok();

    BenchResult {
        mode: "passthrough".to_string(),
        config: format!("j{}", jobs),
        files: files_count,
        pus,
        gen_time,
        compile_time,
        total_time,
        success_rate,
    }
}

fn run_split(
    cfg: &Config,
    src_dir: &Path,
    gen_jobs: usize,
    compile_jobs: usize,
    run_id: &str,
) -> BenchResult {
    let dest_dir = cfg.work_dir.join(format!("split_{}", run_id));
    fs::create_dir_all(&dest_dir).ok();

    // Copy .i files
    let i_files = find_i_files(src_dir);
    let files_count = i_files.len();

    for ifile in &i_files {
        if let Some(name) = ifile.file_name() {
            let dest = dest_dir.join(name);
            fs::copy(ifile, dest).ok();
        }
    }

    let gen_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(gen_jobs)
        .build()
        .unwrap();

    let compile_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(compile_jobs)
        .build()
        .unwrap();

    // Generate split .pu.c files
    let start_gen = Instant::now();

    let dest_i_files = find_i_files(&dest_dir);
    gen_pool.install(|| {
        dest_i_files.par_iter().for_each(|ifile| {
            Command::new(&cfg.precc_bin)
                .arg(ifile)
                .env("PASSTHROUGH_THRESHOLD", "0")
                .env("SPLIT", "1")
                .stderr(Stdio::null())
                .stdout(Stdio::null())
                .status()
                .ok();
        });
    });

    let gen_time = start_gen.elapsed().as_secs_f64();

    // Count generated PUs
    let pu_files = find_pu_files(&dest_dir);
    let pus = pu_files.len();

    // Compile
    let start_compile = Instant::now();
    let ok = compile_pool.install(|| {
        pu_files
            .par_iter()
            .filter(|f| compile_file(f))
            .count()
    });
    let compile_time = start_compile.elapsed().as_secs_f64();

    let total_time = gen_time + compile_time;
    let success_rate = if pus > 0 {
        (ok as f64 * 100.0) / pus as f64
    } else {
        0.0
    };

    fs::remove_dir_all(&dest_dir).ok();

    BenchResult {
        mode: "split".to_string(),
        config: format!("g{}_c{}", gen_jobs, compile_jobs),
        files: files_count,
        pus,
        gen_time,
        compile_time,
        total_time,
        success_rate,
    }
}

fn run_by_size(
    cfg: &Config,
    src_dir: &Path,
    threshold: usize,
    jobs: usize,
    run_id: &str,
) -> BenchResult {
    let dest_dir = cfg.work_dir.join(format!("bysize_{}_{}", threshold, run_id));
    fs::create_dir_all(&dest_dir).ok();

    // Copy .i files
    let i_files = find_i_files(src_dir);
    let files_count = i_files.len();

    for ifile in &i_files {
        if let Some(name) = ifile.file_name() {
            let dest = dest_dir.join(name);
            fs::copy(ifile, dest).ok();
        }
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .unwrap();

    // Process each file based on size
    let start_gen = Instant::now();

    let dest_i_files = find_i_files(&dest_dir);
    pool.install(|| {
        dest_i_files.par_iter().for_each(|ifile| {
            let loc = count_loc(ifile);

            if loc > threshold {
                // Large file: split
                Command::new(&cfg.precc_bin)
                    .arg(ifile)
                    .env("PASSTHROUGH_THRESHOLD", "0")
                    .env("SPLIT", "1")
                    .stderr(Stdio::null())
                    .stdout(Stdio::null())
                    .status()
                    .ok();
            } else {
                // Small file: passthrough
                Command::new(&cfg.precc_bin)
                    .arg(ifile)
                    .env("SPLIT", "0")
                    .stderr(Stdio::null())
                    .stdout(Stdio::null())
                    .status()
                    .ok();
            }
        });
    });

    let gen_time = start_gen.elapsed().as_secs_f64();

    // Count generated PUs
    let pu_files = find_pu_files(&dest_dir);
    let pus = pu_files.len();

    // Compile
    let start_compile = Instant::now();
    let ok = pool.install(|| {
        pu_files
            .par_iter()
            .filter(|f| compile_file(f))
            .count()
    });
    let compile_time = start_compile.elapsed().as_secs_f64();

    let total_time = gen_time + compile_time;
    let success_rate = if pus > 0 {
        (ok as f64 * 100.0) / pus as f64
    } else {
        0.0
    };

    fs::remove_dir_all(&dest_dir).ok();

    BenchResult {
        mode: "by_size".to_string(),
        config: format!("loc{}_j{}", threshold, jobs),
        files: files_count,
        pus,
        gen_time,
        compile_time,
        total_time,
        success_rate,
    }
}

fn average_results(results: &[BenchResult]) -> BenchResult {
    if results.is_empty() {
        return BenchResult {
            mode: String::new(),
            config: String::new(),
            files: 0,
            pus: 0,
            gen_time: 0.0,
            compile_time: 0.0,
            total_time: 0.0,
            success_rate: 0.0,
        };
    }

    let n = results.len() as f64;
    let sum_gen: f64 = results.iter().map(|r| r.gen_time).sum();
    let sum_compile: f64 = results.iter().map(|r| r.compile_time).sum();
    let sum_total: f64 = results.iter().map(|r| r.total_time).sum();

    BenchResult {
        mode: results[0].mode.clone(),
        config: results[0].config.clone(),
        files: results[0].files,
        pus: results[0].pus,
        gen_time: sum_gen / n,
        compile_time: sum_compile / n,
        total_time: sum_total / n,
        success_rate: results[0].success_rate,
    }
}

fn print_usage() {
    eprintln!(
        r#"Usage: benchmark_modes [OPTIONS]

Benchmark precc modes on vim codebase and measure speedup ratios.

Options:
    -r, --runs N        Number of runs per configuration (default: 3)
    -j, --jobs N        Max parallel jobs (default: num CPUs)
    -o, --output FILE   Output CSV file (default: results.csv in work dir)
    -k, --keep          Keep work directory after completion
    -h, --help          Show this help message

Modes tested:
    - baseline:     Direct gcc compilation (make -jN equivalent)
    - passthrough:  precc without splitting (SPLIT=0)
    - split:        precc split mode with various job configs
    - by-size:      Split only files > threshold LOC
"#
    );
}

fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();
    let mut num_runs = 3;
    let mut max_jobs = get_num_cpus();
    let mut keep_work = false;
    let mut output_file: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-r" | "--runs" => {
                i += 1;
                if i < args.len() {
                    num_runs = args[i].parse().unwrap_or(3);
                }
            }
            "-j" | "--jobs" => {
                i += 1;
                if i < args.len() {
                    max_jobs = args[i].parse().unwrap_or(get_num_cpus());
                }
            }
            "-o" | "--output" => {
                i += 1;
                if i < args.len() {
                    output_file = Some(PathBuf::from(&args[i]));
                }
            }
            "-k" | "--keep" => {
                keep_work = true;
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let half_jobs = max_jobs / 2;
    let quarter_jobs = max_jobs / 4;

    // Get project root (assuming binary is in target/release or target/debug)
    let exe_path = env::current_exe().unwrap_or_default();
    let project_root = exe_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| env::current_dir().unwrap());

    let work_dir = PathBuf::from(format!("/tmp/precc_benchmark_{}", std::process::id()));
    let results_file = output_file.unwrap_or_else(|| work_dir.join("results.csv"));

    Config {
        num_runs,
        max_jobs,
        half_jobs,
        quarter_jobs,
        keep_work,
        results_file,
        work_dir,
        vim_src: project_root.join("tests/vim/src"),
        precc_bin: project_root.join("target/release/precc"),
    }
}

fn main() {
    let cfg = parse_args();

    eprintln!(
        "{BOLD}{BLUE}═══════════════════════════════════════════════════════════════{NC}"
    );
    eprintln!(
        "{BOLD}{BLUE}          PRECC MODE BENCHMARK - VIM CODEBASE                  {NC}"
    );
    eprintln!(
        "{BOLD}{BLUE}═══════════════════════════════════════════════════════════════{NC}"
    );
    eprintln!();

    // Check prerequisites
    if !cfg.precc_bin.exists() {
        eprintln!(
            "{}Error: precc binary not found at {:?}{}",
            RED, cfg.precc_bin, NC
        );
        eprintln!("Run: cargo build --release");
        std::process::exit(1);
    }

    if !cfg.vim_src.exists() {
        eprintln!(
            "{}Error: vim source not found at {:?}{}",
            RED, cfg.vim_src, NC
        );
        std::process::exit(1);
    }

    fs::create_dir_all(&cfg.work_dir).expect("Failed to create work directory");

    log(&format!("Work directory: {:?}", cfg.work_dir));
    log(&format!("Max jobs: {}", cfg.max_jobs));
    log(&format!("Runs per config: {}", cfg.num_runs));
    eprintln!();

    // Initialize results file
    let mut results_file = File::create(&cfg.results_file).expect("Failed to create results file");
    writeln!(
        results_file,
        "mode,config,files,pus,gen_time,compile_time,total_time,success_rate"
    )
    .ok();

    // Preprocess files once
    let prep_dir = cfg.work_dir.join("preprocessed");
    let num_files = preprocess_files(&cfg, &prep_dir);
    log(&format!("Preprocessed {} files", num_files));
    eprintln!();

    let mut all_results: Vec<BenchResult> = Vec::new();

    // ===== BASELINE =====
    eprintln!(
        "{BOLD}{YELLOW}[1/6] Running baseline (direct gcc compilation)...{NC}"
    );
    let mut baseline_results = Vec::new();
    for run in 1..=cfg.num_runs {
        log(&format!("  Run {}/{}", run, cfg.num_runs));
        let result = run_baseline(&cfg, &prep_dir, cfg.max_jobs, run);
        baseline_results.push(result);
    }
    let baseline_avg = average_results(&baseline_results);
    writeln!(results_file, "{}", baseline_avg.to_csv()).ok();
    let baseline_time = baseline_avg.total_time;
    log_result(&format!("Baseline: {:.3}s", baseline_time));
    all_results.push(baseline_avg);
    eprintln!();

    // ===== PASSTHROUGH =====
    eprintln!("{BOLD}{YELLOW}[2/6] Running passthrough mode...{NC}");
    for &jobs in &[cfg.max_jobs, cfg.half_jobs, cfg.quarter_jobs] {
        if jobs == 0 {
            continue;
        }
        let mut passthrough_results = Vec::new();
        for run in 1..=cfg.num_runs {
            log(&format!("  j{} - Run {}/{}", jobs, run, cfg.num_runs));
            let result = run_passthrough(&cfg, &prep_dir, jobs, &format!("{}_{}", jobs, run));
            passthrough_results.push(result);
        }
        let avg = average_results(&passthrough_results);
        writeln!(results_file, "{}", avg.to_csv()).ok();
        let speedup = if avg.total_time > 0.0 {
            baseline_time / avg.total_time
        } else {
            0.0
        };
        log_result(&format!(
            "Passthrough j{}: {:.3}s (speedup: {:.2}x)",
            jobs, avg.total_time, speedup
        ));
        all_results.push(avg);
    }
    eprintln!();

    // ===== SPLIT MODE =====
    eprintln!(
        "{BOLD}{YELLOW}[3/6] Running split mode (various job configs)...{NC}"
    );
    let job_configs = [
        (cfg.max_jobs, cfg.max_jobs),
        (cfg.half_jobs, cfg.max_jobs),
        (cfg.quarter_jobs, cfg.max_jobs),
        (cfg.max_jobs, cfg.half_jobs),
        (1, cfg.max_jobs),
    ];

    for &(gen_j, compile_j) in &job_configs {
        if gen_j == 0 || compile_j == 0 {
            continue;
        }
        let mut split_results = Vec::new();
        for run in 1..=cfg.num_runs {
            log(&format!(
                "  g{}_c{} - Run {}/{}",
                gen_j, compile_j, run, cfg.num_runs
            ));
            let result = run_split(
                &cfg,
                &prep_dir,
                gen_j,
                compile_j,
                &format!("{}_{}_{}", gen_j, compile_j, run),
            );
            split_results.push(result);
        }
        let avg = average_results(&split_results);
        writeln!(results_file, "{}", avg.to_csv()).ok();
        let speedup = if avg.total_time > 0.0 {
            baseline_time / avg.total_time
        } else {
            0.0
        };
        log_result(&format!(
            "Split g{}_c{}: {:.3}s (speedup: {:.2}x)",
            gen_j, compile_j, avg.total_time, speedup
        ));
        all_results.push(avg);
    }
    eprintln!();

    // ===== SIZE-BASED SPLITTING =====
    eprintln!("{BOLD}{YELLOW}[4/6] Running size-based splitting...{NC}");
    let thresholds = [1000, 2000, 5000, 10000, 50000];

    for &threshold in &thresholds {
        let mut bysize_results = Vec::new();
        for run in 1..=cfg.num_runs {
            log(&format!(
                "  LOC>{} - Run {}/{}",
                threshold, run, cfg.num_runs
            ));
            let result = run_by_size(
                &cfg,
                &prep_dir,
                threshold,
                cfg.max_jobs,
                &format!("{}_{}", threshold, run),
            );
            bysize_results.push(result);
        }
        let avg = average_results(&bysize_results);
        writeln!(results_file, "{}", avg.to_csv()).ok();
        let speedup = if avg.total_time > 0.0 {
            baseline_time / avg.total_time
        } else {
            0.0
        };
        log_result(&format!(
            "By-size LOC>{}: {:.3}s (speedup: {:.2}x)",
            threshold, avg.total_time, speedup
        ));
        all_results.push(avg);
    }
    eprintln!();

    // ===== SEQUENTIAL BASELINE =====
    eprintln!("{BOLD}{YELLOW}[5/6] Running sequential baseline (j1)...{NC}");
    let mut seq_results = Vec::new();
    for run in 1..=cfg.num_runs {
        log(&format!("  Run {}/{}", run, cfg.num_runs));
        let result = run_baseline(&cfg, &prep_dir, 1, run);
        seq_results.push(result);
    }
    let seq_avg = average_results(&seq_results);
    writeln!(results_file, "{}", seq_avg.to_csv()).ok();
    let seq_time = seq_avg.total_time;
    log_result(&format!("Sequential baseline: {:.3}s", seq_time));
    all_results.push(seq_avg);
    eprintln!();

    // ===== SPLIT SEQUENTIAL COMPILE =====
    eprintln!(
        "{BOLD}{YELLOW}[6/6] Running split with sequential compile...{NC}"
    );
    let mut split_seq_results = Vec::new();
    for run in 1..=cfg.num_runs {
        log(&format!("  Run {}/{}", run, cfg.num_runs));
        let result = run_split(
            &cfg,
            &prep_dir,
            cfg.max_jobs,
            1,
            &format!("seq_compile_{}", run),
        );
        split_seq_results.push(result);
    }
    let split_seq_avg = average_results(&split_seq_results);
    writeln!(results_file, "{}", split_seq_avg.to_csv()).ok();
    let split_seq_time = split_seq_avg.total_time;
    let speedup = if split_seq_time > 0.0 {
        seq_time / split_seq_time
    } else {
        0.0
    };
    log_result(&format!(
        "Split seq compile: {:.3}s (vs seq baseline speedup: {:.2}x)",
        split_seq_time, speedup
    ));
    all_results.push(split_seq_avg);
    eprintln!();

    // ===== RESULTS SUMMARY =====
    eprintln!(
        "{BOLD}{BLUE}═══════════════════════════════════════════════════════════════{NC}"
    );
    eprintln!(
        "{BOLD}{BLUE}                    RESULTS SUMMARY                             {NC}"
    );
    eprintln!(
        "{BOLD}{BLUE}═══════════════════════════════════════════════════════════════{NC}"
    );
    eprintln!();

    eprintln!("{BOLD}Results ordered by speedup (vs parallel baseline):{NC}");
    eprintln!();

    // Sort by speedup (descending)
    let mut results_with_speedup: Vec<(BenchResult, f64)> = all_results
        .iter()
        .map(|r| {
            let speedup = if r.total_time > 0.0 {
                baseline_time / r.total_time
            } else {
                0.0
            };
            (r.clone(), speedup)
        })
        .collect();
    results_with_speedup.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    eprintln!(
        "{:<25} {:>8} {:>8} {:>10} {:>10} {:>8}",
        "Mode", "Files", "PUs", "Total(s)", "Speedup", "Success"
    );
    eprintln!(
        "{:<25} {:>8} {:>8} {:>10} {:>10} {:>8}",
        "-------------------------",
        "--------",
        "--------",
        "----------",
        "----------",
        "--------"
    );

    for (result, speedup) in &results_with_speedup {
        eprintln!(
            "{:<25} {:>8} {:>8} {:>10.3} {:>9.2}x {:>7.0}%",
            format!("{}:{}", result.mode, result.config),
            result.files,
            result.pus,
            result.total_time,
            speedup,
            result.success_rate
        );
    }

    eprintln!();
    eprintln!("{BOLD}Results saved to:{NC} {:?}", cfg.results_file);

    // Generate detailed CSV with speedup
    let detailed_file = cfg
        .results_file
        .with_file_name(format!(
            "{}_detailed.csv",
            cfg.results_file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("results")
        ));

    if let Ok(mut detailed) = File::create(&detailed_file) {
        writeln!(
            detailed,
            "mode,config,files,pus,gen_time,compile_time,total_time,success_rate,speedup_vs_baseline"
        )
        .ok();
        for (result, speedup) in &results_with_speedup {
            writeln!(detailed, "{},{:.4}", result.to_csv(), speedup).ok();
        }
        eprintln!("{BOLD}Detailed results:{NC} {:?}", detailed_file);
    }

    if cfg.keep_work {
        eprintln!("{BOLD}Work directory:{NC} {:?}", cfg.work_dir);
    } else {
        fs::remove_dir_all(&cfg.work_dir).ok();
    }
}
