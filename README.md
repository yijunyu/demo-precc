# precc
A demo of the `precc` precompiler for C/C++ (e.g., GCC, Clang), which is
fundamentally a high-performance C/C++ file splitter that breaks large source
files into smaller compilation units based on function and variable
dependencies.

## Usage

```bash
cargo build --release # Build release version
cargo run --release # the Sqlite3 amalgation test case
cargo run --release -- --vim # the Vim test case
UNITY=1 SPLIT=1 cargo run --release -- --vim # the Vim test case with optimal balance of size and split of compilation units
bin/precc <filename.i> # Process without splitting
UNITY=1 SPLIT=1 bin/precc <filename.i> # Process with splitting
```
