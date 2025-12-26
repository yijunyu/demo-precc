# demo-precc

A demonstration of `precc` - a high-performance C/C++ precompiler that splits large source files into smaller, independently compilable units.

## Overview

`precc` achieves ~200x performance improvement over traditional approaches by splitting large C files into smaller compilation units based on function and variable dependencies extracted via `ctags` analysis.

## Quick Start

### Download Binary

Download the pre-built binary for your platform from the [Releases](https://github.com/yijunyu/demo-precc/releases) page.

### Usage

```bash
# Basic usage - analyze a preprocessed C file
./precc input.i

# Split mode - generate multiple processing units
SPLIT=1 ./precc input.i

# This generates: input.i_1.pu.c, input.i_2.pu.c, etc.
```

### Compile Split Units

```bash
# Compile each processing unit
for f in input.i_*.pu.c; do
    gcc -c "$f"
done
```

## Example

The `examples/` directory contains sample C files for testing:

```bash
cd examples
gcc -E hello.c -o hello.i
SPLIT=1 ../bin/precc hello.i
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `SPLIT` | Enable split mode (generates `<file>_N.pu.c` files) |
| `PU_FILTER` | Generate only specific PUs (comma-separated UIDs) |
| `START_PU` | Skip PUs before this UID |
| `PASSTHROUGH_THRESHOLD` | Set to 0 to force splitting small files |

## Test Results

| Test | Result |
|------|--------|
| Vim (125 files) | 8311/8311 PUs (100%) |
| SQLite (260K LOC) | 2503/2503 PUs (100%) |

## Related Projects

- [cargo-slicer](https://github.com/yijunyu/cargo-slicer) - Semantic slicing tool for Rust crates
- [precc](https://github.com/anthropics/precc) - Full source code (if available)

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
