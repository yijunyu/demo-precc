//! precc-sweep — Rust experiment sweep for ML training data collection.
//!
//! Runs precc in-process across all config axes, captures TIMING lines from
//! stderr, records results directly into the experiment DB.
//!
//! Advantages over bash scripts:
//!   - In-process main_wrapper calls (no subprocess fork/exec overhead ~50ms each)
//!   - Stderr capture via OS pipe instead of temp files
//!   - Adaptive sampling: fewer reps for slow configs, more for fast ones
//!   - Early stopping: skip axes that are clearly uninteresting
//!   - Progress reporting with ETA
//!   - Direct DB insertion (no shell quoting issues)
//!
//! Usage:
//!   precc-sweep [options]
//!
//! Options:
//!   --ifile <path>          .i file to sweep (repeatable; default: auto-detect)
//!   --project <name>        Project label (default: basename of parent dir)
//!   --baseline-cmd <cmd>    Baseline timing command (default: "gcc -O2 -g -c {file} -o /dev/null")
//!   --baseline-jobs <N>     Parallelism for multi-file baseline (default: nproc)
//!   --reps <N>              Target repetitions for fast configs (default: 3)
//!   --max-slow-s <secs>     Skip configs slower than this × baseline (default: 10.0)
//!   --db <path>             DB path (default: ~/.precc/experiments.db)
//!   --dry-run               Print configs without running
//!   --jobs-sweep <j,j,...>  Job counts to sweep (default: 1,4,8,16,24,48)
//!   --no-split              Skip split-mode experiments
//!   --no-pch                Skip PCH-mode experiments
//!   --no-passthrough        Skip passthrough experiments
//!   --summary               Print DB summary after sweep

use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use precc::experiment_db::{open_db, insert_experiment, ExperimentRecord, default_db_path};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct SweepConfig {
    ifiles: Vec<PathBuf>,
    project: String,
    baseline_cmd: Option<String>,
    baseline_jobs: usize,
    reps: usize,
    max_slow_factor: f64,
    db_path: PathBuf,
    dry_run: bool,
    jobs_sweep: Vec<usize>,
    do_split: bool,
    do_pch: bool,
    do_passthrough: bool,
    do_split_threshold: bool,
    print_summary: bool,
    pch_min_src_frac_sweep: Vec<f64>,
    passthrough_threshold_sweep: Vec<usize>,
    split_threshold_sweep: Vec<usize>,
}

impl Default for SweepConfig {
    fn default() -> Self {
        let ncpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
        Self {
            ifiles: Vec::new(),
            project: String::new(),
            baseline_cmd: None,
            baseline_jobs: ncpus,
            reps: 3,
            max_slow_factor: 10.0,
            db_path: default_db_path(),
            dry_run: false,
            jobs_sweep: vec![1, 4, 8, 16, 24, 48],
            do_split: true,
            do_pch: true,
            do_passthrough: true,
            do_split_threshold: true,
            print_summary: false,
            pch_min_src_frac_sweep: vec![0.0, 0.1, 0.2, 0.3, 0.5, 0.7],
            passthrough_threshold_sweep: vec![0, 10, 50, 100, 500, 5000],
            split_threshold_sweep: vec![10, 30, 50, 100, 200, 500],
        }
    }
}

// ── Timing capture ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
struct RunResult {
    wall_s: f64,
    ctags_s: Option<f64>,
    dep_s: Option<f64>,
    total_s: Option<f64>,
    stderr_lines: Vec<String>,
    pch_was_used: bool,
    effective_strategy: String,
}

// ── Subprocess timing ─────────────────────────────────────────────────────────

/// Run precc as a subprocess, capture stderr TIMING lines.
/// Median of `reps` runs (skips if wall > max_wall_s after first run).
fn run_subprocess(
    precc_bin: &Path,
    ifile: &Path,
    env_vars: &[(&str, &str)],
    reps: usize,
    max_wall_s: f64,
) -> RunResult {
    let mut wall_times: Vec<f64> = Vec::new();
    let mut last_result = RunResult::default();

    // Stem for glob patterns (e.g. "path/to/file.i")
    let ifile_str = ifile.to_string_lossy();

    for rep in 0..reps {
        // Clean up stale .pu.c files so precc can't skip re-generation via cache
        let basename_str = ifile.file_name().map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ifile_str.to_string());
        let cleanup_patterns = [
            format!("{}.pu.c", ifile_str),
            format!("{}_*.pu.c", ifile_str),
            format!("{}.bundle_*.pu.c", ifile_str),  // bundle next to input
            format!("{}.bundle_*.pu.c", basename_str), // bundle in cwd (PCH mode)
        ];
        for pat in &cleanup_patterns {
            if let Ok(entries) = glob::glob(pat) {
                for p in entries.flatten() {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }

        let t0 = Instant::now();
        let mut cmd = std::process::Command::new(precc_bin);
        cmd.arg(ifile)
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::piped());
        for &(k, v) in env_vars {
            cmd.env(k, v);
        }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  [sweep] spawn error: {}", e);
                return RunResult::default();
            }
        };
        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(_) => { wall_times.push(t0.elapsed().as_secs_f64()); continue; }
        };
        let wall_s = t0.elapsed().as_secs_f64();
        wall_times.push(wall_s);

        // Early-stop: if first run is already too slow, don't repeat
        if rep == 0 && wall_s > max_wall_s {
            last_result = parse_stderr_output(&output.stderr, wall_s);
            break;
        }
        if rep == reps - 1 || wall_s <= max_wall_s {
            last_result = parse_stderr_output(&output.stderr, wall_s);
        }
    }

    // Use median wall time
    wall_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_wall = wall_times[wall_times.len() / 2];
    last_result.wall_s = median_wall;
    last_result
}

fn parse_stderr_output(stderr: &[u8], wall_s: f64) -> RunResult {
    let text = String::from_utf8_lossy(stderr);
    let mut r = RunResult { wall_s, ..Default::default() };

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("[TIMING] ctags processing: ") {
            r.ctags_s = rest.trim_end_matches('s').parse().ok();
        } else if let Some(rest) = line.strip_prefix("[TIMING] dependency computation: ") {
            r.dep_s = rest.trim_end_matches('s').parse().ok();
        } else if let Some(rest) = line.strip_prefix("[TIMING] TOTAL: ") {
            r.total_s = rest.trim_end_matches('s').parse().ok();
        } else if line.contains("PCH: wrote") {
            r.pch_was_used = true;
        }
        r.stderr_lines.push(line.to_string());
    }

    // Infer effective strategy from output
    if r.pch_was_used {
        r.effective_strategy = "pch".to_string();
    } else if text.contains("passthrough") || text.contains("Passthrough") {
        r.effective_strategy = "passthrough".to_string();
    } else {
        r.effective_strategy = "split".to_string();
    }

    r
}

// ── Baseline timing ───────────────────────────────────────────────────────────

fn measure_baseline(ifile: &Path, reps: usize) -> f64 {
    let gcc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let mut times = Vec::new();
    for _ in 0..reps {
        let t0 = Instant::now();
        let _ = std::process::Command::new(&gcc)
            .args(["-O2", "-g", "-c"])
            .arg(ifile)
            .args(["-o", "/dev/null"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        times.push(t0.elapsed().as_secs_f64());
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times[times.len() / 2]
}

// ── Downstream compile of .pu.c files ────────────────────────────────────────

/// After precc runs, compile all .pu.c files it produced in parallel.
/// Returns (wall_s, n_files) — wall time is from job submission to last completion.
fn compile_pu_files(ifile: &Path, jobs: usize) -> (f64, usize) {
    let stem = ifile.to_string_lossy();
    // basename of the input file (e.g. "sqlite3.i" from "tests/sqlite3/sqlite3.i")
    let basename = ifile.file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| stem.to_string());
    // precc produces one of:
    //   <file>.pu.c               — passthrough (1 file, next to input)
    //   <file>_NNN.pu.c           — split mode (many files, next to input)
    //   {basename}.bundle_N.pu.c  — PCH/bundle mode (written to cwd, not input dir)
    let pattern_single      = format!("{}.pu.c", stem);
    let pattern_split       = format!("{}_*.pu.c", stem);
    let pattern_bundle_cwd  = format!("{}.bundle_*.pu.c", basename);   // in cwd
    let pattern_bundle_dir  = format!("{}.bundle_*.pu.c", stem);       // next to input

    let mut pu_files: Vec<std::path::PathBuf> = Vec::new();
    // Check split pattern first (most common for split mode)
    if let Ok(entries) = glob::glob(&pattern_split) {
        pu_files.extend(entries.flatten());
    }
    // Check bundle patterns (PCH mode) — precc writes to cwd
    for pat in &[&pattern_bundle_cwd, &pattern_bundle_dir] {
        if let Ok(entries) = glob::glob(pat) {
            pu_files.extend(entries.flatten());
        }
    }
    // Fall back to single passthrough file
    if pu_files.is_empty() {
        if let Ok(entries) = glob::glob(&pattern_single) {
            pu_files.extend(entries.flatten());
        }
    }
    pu_files.dedup();

    if pu_files.is_empty() {
        return (0.0, 0);
    }

    let gcc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let n = pu_files.len();
    let t0 = Instant::now();

    // Compile in parallel using rayon with the same job count precc used
    use std::sync::atomic::{AtomicUsize, Ordering};
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap());
    let _ok = AtomicUsize::new(0);
    pool.install(|| {
        use rayon::prelude::*;
        pu_files.par_iter().for_each(|pu| {
            let _ = std::process::Command::new(&gcc)
                .args(["-O2", "-g", "-c"])
                .arg(pu)
                .args(["-o", "/dev/null"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    });

    (t0.elapsed().as_secs_f64(), n)
}

// ── File metrics ──────────────────────────────────────────────────────────────

fn file_metrics(ifile: &Path) -> (f64, i64, Option<i64>) {
    // Use analyze_strategy with per_file=true on a single file
    let path_str = ifile.to_string_lossy().to_string();
    let r = precc::analyze_strategy(&[path_str.as_str()], 1, None, None, true);
    let (src_frac, fn_braces) = r.per_file.first()
        .map(|e| (e.src_frac, e.fn_braces as i64))
        .unwrap_or((r.mean_src_frac, r.mean_fn_braces as i64));
    let file_size = std::fs::metadata(ifile).map(|m| m.len() as i64).ok();
    (src_frac, fn_braces, file_size)
}

fn count_lines(ifile: &Path) -> Option<i64> {
    use std::io::BufRead;
    let f = std::fs::File::open(ifile).ok()?;
    let n = std::io::BufReader::new(f).lines().count();
    Some(n as i64)
}

// ── Experiment runner ─────────────────────────────────────────────────────────

struct Sweeper {
    cfg: SweepConfig,
    precc_bin: PathBuf,
    conn: rusqlite::Connection,
    total_configs: usize,
    done: usize,
    t_start: Instant,
}

impl Sweeper {
    fn new(cfg: SweepConfig) -> Result<Self, String> {
        let precc_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("precc")))
            .filter(|p| p.exists())
            .unwrap_or_else(|| {
                // Try ~/.cargo/bin/precc
                let home = env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".cargo/bin/precc")
            });

        let conn = open_db(&cfg.db_path)?;
        Ok(Self {
            cfg,
            precc_bin,
            conn,
            total_configs: 0,
            done: 0,
            t_start: Instant::now(),
        })
    }

    fn run_all(&mut self) {
        let ifiles = self.cfg.ifiles.clone();
        for ifile in &ifiles {
            self.sweep_file(ifile);
        }
        if self.cfg.print_summary {
            self.print_summary();
        }
    }

    fn sweep_file(&mut self, ifile: &Path) {
        let fname = ifile.file_name().unwrap_or_default().to_string_lossy().to_string();
        let project = if self.cfg.project.is_empty() {
            ifile.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        } else {
            self.cfg.project.clone()
        };

        eprintln!("\n── {} / {} ──", project, fname);

        // Measure file metrics once
        eprint!("  measuring file metrics...");
        let (src_frac, fn_braces, file_size) = file_metrics(ifile);
        let total_lines = count_lines(ifile);
        eprintln!(" src_frac={:.3} fn_braces={} size={}KB",
            src_frac, fn_braces,
            file_size.unwrap_or(0) / 1024);

        // Baseline (3 reps for accurate measurement)
        eprint!("  measuring baseline...");
        let baseline_s = if self.cfg.dry_run { 1.0 } else { measure_baseline(ifile, 3) };
        eprintln!(" {:.3}s", baseline_s);

        let max_wall_s = baseline_s * self.cfg.max_slow_factor;

        // ── Passthrough ───────────────────────────────────────────────────────
        if self.cfg.do_passthrough {
            // Set threshold > file size so precc always passes through (no splitting).
            // PASSTHROUGH_THRESHOLD=N means "skip splitting for files < N bytes".
            let pt_val = file_size.map(|s| (s + 1).to_string())
                .unwrap_or_else(|| "99999999".to_string());
            let pt_note = format!("passthrough (PASSTHROUGH_THRESHOLD={})", pt_val);
            self.run_config(ifile, &fname, &project, baseline_s, max_wall_s,
                src_frac, fn_braces, file_size, total_lines,
                &[("PASSTHROUGH_THRESHOLD", &pt_val)],
                "passthrough", false, false,
                &pt_note);
        }

        // ── Split × jobs ──────────────────────────────────────────────────────
        if self.cfg.do_split {
            let jobs_sweep = self.cfg.jobs_sweep.clone();
            for &jobs in &jobs_sweep {
                let jobs_s = jobs.to_string();
                let ncpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
                // Skip j>ncpus configs (no additional parallelism)
                if jobs > ncpus * 2 { continue; }
                self.run_config(ifile, &fname, &project, baseline_s, max_wall_s,
                    src_frac, fn_braces, file_size, total_lines,
                    &[("SPLIT", "1"), ("JOBS", &jobs_s)],
                    "split", false, true, &format!("split j{}", jobs));
            }
        }

        // ── PCH × jobs ────────────────────────────────────────────────────────
        if self.cfg.do_pch {
            let jobs_sweep = self.cfg.jobs_sweep.clone();
            let ncpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
            for &jobs in &jobs_sweep {
                if jobs > ncpus * 2 { continue; }
                let jobs_s = jobs.to_string();
                self.run_config(ifile, &fname, &project, baseline_s, max_wall_s,
                    src_frac, fn_braces, file_size, total_lines,
                    &[("PRECC_PCH", "1"), ("SPLIT", "1"), ("JOBS", &jobs_s)],
                    "pch", true, true, &format!("PCH j{}", jobs));
            }

            // ── PCH × pch_min_src_frac ────────────────────────────────────────
            let ncpus_s = ncpus.to_string();
            let pch_min_fracs = self.cfg.pch_min_src_frac_sweep.clone();
            for min_frac in pch_min_fracs {
                let frac_s = format!("{}", min_frac);
                self.run_config(ifile, &fname, &project, baseline_s, max_wall_s,
                    src_frac, fn_braces, file_size, total_lines,
                    &[("PRECC_PCH", "1"), ("SPLIT", "1"),
                      ("JOBS", &ncpus_s),
                      ("PRECC_PCH_MIN_SRC_FRAC", &frac_s)],
                    "pch", true, true,
                    &format!("PCH pch_min_src_frac={} j{}", min_frac, ncpus));
            }

            // ── PCH × passthrough_threshold ───────────────────────────────────
            let pch_thresholds = self.cfg.passthrough_threshold_sweep.clone();
            for thresh in pch_thresholds {
                let thresh_s = thresh.to_string();
                self.run_config(ifile, &fname, &project, baseline_s, max_wall_s,
                    src_frac, fn_braces, file_size, total_lines,
                    &[("PRECC_PCH", "1"), ("SPLIT", "1"),
                      ("JOBS", &ncpus_s),
                      ("PASSTHROUGH_THRESHOLD", &thresh_s)],
                    "pch", true, true,
                    &format!("PCH passthrough_threshold={} j{}", thresh, ncpus));
            }
        }

        // ── Split × passthrough_threshold ─────────────────────────────────────
        if self.cfg.do_split && self.cfg.do_split_threshold {
            let ncpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
            let ncpus_s = ncpus.to_string();
            let split_thresholds = self.cfg.split_threshold_sweep.clone();
            for thresh in split_thresholds {
                let thresh_s = thresh.to_string();
                self.run_config(ifile, &fname, &project, baseline_s, max_wall_s,
                    src_frac, fn_braces, file_size, total_lines,
                    &[("SPLIT", "1"), ("JOBS", &ncpus_s),
                      ("PASSTHROUGH_THRESHOLD", &thresh_s)],
                    "split", false, true,
                    &format!("split passthrough_threshold={} j{}", thresh, ncpus));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_config(
        &mut self,
        ifile: &Path,
        fname: &str,
        project: &str,
        baseline_s: f64,
        max_wall_s: f64,
        src_frac: f64,
        fn_braces: i64,
        file_size: Option<i64>,
        total_lines: Option<i64>,
        env_vars: &[(&str, &str)],
        strategy_hint: &str,
        use_pch: bool,
        use_split: bool,
        note: &str,
    ) {
        self.done += 1;
        let jobs: i64 = env_vars.iter()
            .find(|&&(k, _)| k == "JOBS")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(1);
        let pch_min: Option<f64> = env_vars.iter()
            .find(|&&(k, _)| k == "PRECC_PCH_MIN_SRC_FRAC")
            .and_then(|(_, v)| v.parse().ok());
        let pass_thresh: Option<i64> = env_vars.iter()
            .find(|&&(k, _)| k == "PASSTHROUGH_THRESHOLD")
            .and_then(|(_, v)| v.parse().ok());

        let eta_s = if self.done > 1 {
            let elapsed = self.t_start.elapsed().as_secs_f64();
            let per = elapsed / self.done as f64;
            let remain = self.total_configs.saturating_sub(self.done);
            format!(" ETA~{:.0}s", per * remain as f64)
        } else { String::new() };

        eprint!("  [{}/{}{}] {}...", self.done, self.total_configs, eta_s, note);

        if self.cfg.dry_run {
            eprintln!(" [dry-run]");
            return;
        }

        let reps = if max_wall_s < 2.0 { self.cfg.reps } else {
            // Slow config: fewer reps
            (self.cfg.reps as f64 * (2.0 / max_wall_s).min(1.0)).max(1.0) as usize
        };

        let result = run_subprocess(&self.precc_bin, ifile, env_vars, reps, max_wall_s);

        let effective_strategy = if result.effective_strategy.is_empty() {
            strategy_hint.to_string()
        } else {
            result.effective_strategy.clone()
        };

        // Compile the .pu.c files precc produced (end-to-end measurement)
        let (compile_s, n_pu) = compile_pu_files(ifile, jobs as usize);

        let total_s = result.wall_s + compile_s;
        let speedup = if total_s > 0.0 { baseline_s / total_s } else { 0.0 };
        eprintln!(" precc={:.3}s  gcc={:.3}s  total={:.3}s  {:.2}x  ({}, {} .pu.c)",
            result.wall_s, compile_s, total_s, speedup, effective_strategy, n_pu);

        let rec = ExperimentRecord {
            precc_version: None,  // auto-stamped
            git_rev: None,        // auto-stamped
            project: project.to_string(),
            filename: fname.to_string(),
            file_size_bytes: file_size,
            total_lines,
            src_frac,
            fn_braces,
            n_headers: None,
            strategy: effective_strategy,
            use_pch,
            use_split,
            jobs,
            split_count: Some(n_pu as i64),
            passthrough_threshold: pass_thresh,
            pch_min_src_frac: pch_min,
            precc_time_s: result.wall_s,
            baseline_time_s: Some(baseline_s),
            ctags_time_s: result.ctags_s,
            dep_compute_time_s: result.dep_s,
            compile_time_s: Some(compile_s),
            used_config_file: false,
            config_generated_at: None,
            notes: Some(format!("{} / {}", project, note)),
        };

        match insert_experiment(&self.conn, &rec) {
            Ok(id) => { let _ = id; }
            Err(e) => eprintln!("  [sweep] DB error: {}", e),
        }
    }

    fn count_configs(&mut self, ifiles: &[PathBuf]) {
        let ncpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
        let jobs_count = self.cfg.jobs_sweep.iter().filter(|&&j| j <= ncpus * 2).count();
        let split_thresh_count = if self.cfg.do_split && self.cfg.do_split_threshold {
            self.cfg.split_threshold_sweep.len()
        } else { 0 };
        let per_file = (if self.cfg.do_passthrough { 1 } else { 0 })
            + (if self.cfg.do_split { jobs_count + split_thresh_count } else { 0 })
            + (if self.cfg.do_pch {
                jobs_count
                + self.cfg.pch_min_src_frac_sweep.len()
                + self.cfg.passthrough_threshold_sweep.len()
              } else { 0 });
        self.total_configs = per_file * ifiles.len();
    }

    fn print_summary(&self) {
        eprintln!("\n=== sweep complete: {} configs in {:.1}s ===",
            self.done, self.t_start.elapsed().as_secs_f64());

        // Query DB summary
        let sql = "SELECT project, filename, strategy, \
                          round(avg(speedup),2) as avg_x, \
                          round(max(speedup),2) as best_x, \
                          count(*) as n \
                   FROM experiments \
                   WHERE speedup IS NOT NULL \
                   GROUP BY project, filename, strategy \
                   ORDER BY project, filename, best_x DESC";
        let mut stmt = self.conn.prepare(sql).unwrap();
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        eprintln!("{}", cols.join(" | "));
        eprintln!("{}", "-".repeat(60));
        let rows = stmt.query_map([], |row| {
            let mut v = Vec::new();
            for i in 0..cols.len() {
                let val: String = row.get::<_, rusqlite::types::Value>(i)
                    .map(|x| match x {
                        rusqlite::types::Value::Real(f) => format!("{:.3}", f),
                        rusqlite::types::Value::Integer(n) => n.to_string(),
                        rusqlite::types::Value::Text(s) => s,
                        _ => "NULL".to_string(),
                    }).unwrap_or_default();
                v.push(val);
            }
            Ok(v)
        }).unwrap();
        for row in rows.flatten() { eprintln!("{}", row.join(" | ")); }
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut cfg = SweepConfig::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--ifile" => {
                i += 1;
                if let Some(p) = args.get(i) { cfg.ifiles.push(PathBuf::from(p)); }
            }
            "--project" => {
                i += 1;
                if let Some(p) = args.get(i) { cfg.project = p.clone(); }
            }
            "--baseline-jobs" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse().ok()) {
                    cfg.baseline_jobs = v;
                }
            }
            "--reps" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse().ok()) { cfg.reps = v; }
            }
            "--max-slow-s" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse().ok()) {
                    cfg.max_slow_factor = v;
                }
            }
            "--db" => {
                i += 1;
                if let Some(p) = args.get(i) { cfg.db_path = PathBuf::from(p); }
            }
            "--dir" => {
                i += 1;
                if let Some(d) = args.get(i) {
                    if let Ok(entries) = std::fs::read_dir(d) {
                        let mut files: Vec<PathBuf> = entries.flatten()
                            .filter(|e| e.path().extension().map(|x| x == "i").unwrap_or(false))
                            .map(|e| e.path())
                            .collect();
                        files.sort();
                        cfg.ifiles.extend(files);
                    } else {
                        eprintln!("precc-sweep: cannot read dir: {}", d);
                    }
                }
            }
            "--dry-run" => cfg.dry_run = true,
            "--no-split" => cfg.do_split = false,
            "--no-pch" => cfg.do_pch = false,
            "--no-passthrough" => cfg.do_passthrough = false,
            "--no-split-threshold" => cfg.do_split_threshold = false,
            "--summary" => cfg.print_summary = true,
            "--jobs-sweep" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    cfg.jobs_sweep = s.split(',')
                        .filter_map(|x| x.parse().ok())
                        .collect();
                }
            }
            "--pch-min-src-frac-sweep" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    cfg.pch_min_src_frac_sweep = s.split(',')
                        .filter_map(|x| x.parse().ok())
                        .collect();
                }
            }
            "--passthrough-threshold-sweep" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    cfg.passthrough_threshold_sweep = s.split(',')
                        .filter_map(|x| x.parse().ok())
                        .collect();
                }
            }
            "--split-threshold-sweep" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    cfg.split_threshold_sweep = s.split(',')
                        .filter_map(|x| x.parse().ok())
                        .collect();
                }
            }
            "--help" | "-h" => {
                eprintln!("precc-sweep — systematic experiment sweep for ML training data");
                eprintln!();
                eprintln!("Usage: precc-sweep [--ifile <path>]... [--dir <dir>] [options]");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --ifile <path>                    .i file to sweep (repeatable)");
                eprintln!("  --dir <path>                      Sweep all *.i files in directory");
                eprintln!("  --project <name>                  Project label");
                eprintln!("  --reps <N>                        Reps for fast configs (default: 3)");
                eprintln!("  --max-slow-s <factor>             Skip if > factor×baseline (default: 10)");
                eprintln!("  --jobs-sweep <j,j,...>            Jobs to sweep (default: 1,4,8,16,24,48)");
                eprintln!("  --no-split / --no-pch / --no-passthrough / --no-split-threshold");
                eprintln!("  --pch-min-src-frac-sweep <f,...>  e.g. 0.0,0.1,0.2,0.5");
                eprintln!("  --passthrough-threshold-sweep <n,...>  e.g. 0,10,50,100,500");
                eprintln!("  --split-threshold-sweep <n,...>   e.g. 10,30,50,100,200");
                eprintln!("  --db <path>                       DB path (default: ~/.precc/experiments.db)");
                eprintln!("  --dry-run                         Print configs without running");
                eprintln!("  --summary                         Print summary after sweep");
                std::process::exit(0);
            }
            s if s.ends_with(".i") && Path::new(s).exists() => {
                cfg.ifiles.push(PathBuf::from(s));
            }
            _ => {}
        }
        i += 1;
    }

    // Auto-detect .i files if none given
    if cfg.ifiles.is_empty() {
        let vim_dir = PathBuf::from("/home/y00577373/precc/tests/vim/src");
        let sqlite_i = PathBuf::from("/home/y00577373/precc/tests/sqlite3/sqlite3.i");
        if sqlite_i.exists() { cfg.ifiles.push(sqlite_i); }
        if vim_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&vim_dir) {
                let mut vim_files: Vec<PathBuf> = entries.flatten()
                    .filter(|e| e.path().extension().map(|x| x == "i").unwrap_or(false))
                    .map(|e| e.path())
                    .collect();
                vim_files.sort();
                cfg.ifiles.extend(vim_files);
            }
        }
    }

    if cfg.ifiles.is_empty() {
        eprintln!("precc-sweep: no .i files found. Use --ifile <path>");
        std::process::exit(1);
    }

    eprintln!("precc-sweep: {} file(s), reps={}, max_slow={}×baseline",
        cfg.ifiles.len(), cfg.reps, cfg.max_slow_factor);
    for f in &cfg.ifiles {
        eprintln!("  {}", f.display());
    }

    let ifiles = cfg.ifiles.clone();
    let mut sweeper = Sweeper::new(cfg).unwrap_or_else(|e| {
        eprintln!("precc-sweep: DB error: {}", e);
        std::process::exit(1);
    });
    sweeper.count_configs(&ifiles);
    eprintln!("precc-sweep: {} total configs planned\n", sweeper.total_configs);

    sweeper.run_all();
}
