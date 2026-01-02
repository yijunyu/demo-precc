# demo-precc
Demo of the precompiler for C/C++, which is essentially a high-performance
C/C++ file splitter that breaks large source files into smaller compilation
units based on function and variable dependencies. 
Both GCC and Clang C/C++ compilers are supported. 

## Usage

```bash
cargo run
```


![Demo](assets/demo_1.gif)

The usage information is displayed as follows:

```bash
cargo run -- --help
```


![Demo](assets/demo_2.gif)

## Performance

### SQLite3 Amalgamation (Single Large File)

Testing on SQLite's 9.6MB amalgamation file (~260K LOC) with 48-core parallel compilation:

| Metric | Original | Split | Improvement |
|--------|----------|-------|-------------|
| Compilation time | ~35s | ~6s | **5.8x faster** |
| PUs generated | 1 | 2503 | - |
| Code size (text) | 970.0K | 970.5K | 0.05% diff |

**Why splitting helps**: A single large file cannot be parallelized. Splitting into 2503 PUs enables full utilization of all CPU cores.

### Key Insights

- **Split mode is designed for single large amalgamation files** (like SQLite) where the file cannot otherwise be parallelized
- **Passthrough mode** is appropriate for multi-file projects - files below the threshold (default: 1MB) pass through without splitting
- **Code size is virtually identical** between original and split binaries (verified with stripped executables)
- For incremental builds, split mode enables finer granularity - changing one function only requires recompiling a ~35KB PU vs an entire source file

### Regression Tests

| Test Suite | Result |
|------------|--------|
| SQLite amalgamation | **2503/2503 (100%)** |
| Vim source files (125 files) | **8304/8304 (100%)** |

