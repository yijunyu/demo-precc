# precc

A high-performance C/C++ precompiler that splits large translation units into
smaller parallel compilation units, with cluster-based PCH support for dramatic
build speedups on codebases like SQLite, Vim, and the Linux kernel.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/yijunyu/demo-precc/main/install.sh | sh
```

## Usage

```bash
# Split a preprocessed C file into parallel compilation units
precc file.i

# Cluster-PCH mode (groups functions by header dependency, generates shared PCH)
PRECC_PCH_CLUSTER=1 precc file.i

# Sweep compilation strategies to find optimal configuration
precc-sweep --ifile file.i --project myproject --reps 3 --summary
```

## Platforms

| Platform | Architecture |
|----------|-------------|
| Linux    | x86_64      |
| macOS    | aarch64     |
