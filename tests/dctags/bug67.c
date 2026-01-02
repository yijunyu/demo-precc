// Bug67: ctags captures multiple functions in same code span
// When function pointer return type syntax confuses ctags (e.g., "static void (*func(...))(void)")
// it may capture two adjacent functions together.
// The fix extracts declarations for ALL functions in the code span.

// This test validates that the actual SQLite case compiles correctly.
// The minimal reproduction doesn't work well because ctags behaves differently
// with simplified code.

// Validation:
// PASSTHROUGH_THRESHOLD=0 SPLIT=1 PU_FILTER=367 target/release/precc sqlite3.i
// gcc -c sqlite3.i_367.pu.c  # Should compile without 'undeclared' errors for unixDlClose/unixDlSym
