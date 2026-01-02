// Bug65: Extern function declarations missing from PU output
//
// Problem: Functions like close(), read(), write() from <unistd.h> were not
// being included in the extern declarations section of split PU files, causing
// compilation errors like:
//   error: 'close' undeclared here (not in a function)
//
// Root cause: When computing which extern functions were "already declared",
// the code included prototype PUs (e.g., "prototype:close:/path/to/file.i") in
// the check. These prototype PUs contain the original extern declarations from
// the preprocessed file (e.g., "extern int close (int __fd);"), so functions
// like close/read/write were incorrectly marked as "already declared" and
// skipped.
//
// However, prototype PUs are NOT output to the final PU file - they are only
// used for dependency tracking. So their extern declarations don't actually
// appear in the output, causing the undeclared identifier errors.
//
// Fix: Skip prototype PUs when computing the "already_declared" set for extern
// function declarations. This ensures that system functions referenced in the
// code get their simplified extern declarations added.
//
// Test: This test case represents a minimal version of the SQLite aSyscall[]
// array pattern that triggered the bug.
//
// Usage:
//   gcc -E bug65.c -o bug65.i
//   PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug65.i
//   gcc -c bug65.i_*.pu.c

// Simulate a function pointer type
typedef void (*syscall_ptr)(void);

// Simulate system function declarations (these come from <unistd.h> etc.)
extern int close(int fd);
extern int read(int fd, void *buf, unsigned long nbytes);
extern int write(int fd, const void *buf, unsigned long nbytes);
extern int access(const char *path, int mode);
extern char *getcwd(char *buf, unsigned long size);

// A struct to hold syscall function pointers (like SQLite's unix_syscall)
struct unix_syscall {
    const char *zName;
    syscall_ptr pCurrent;
    syscall_ptr pDefault;
};

// The problematic pattern: array of syscall function pointers
// These functions are referenced as values but not called directly in code
static struct unix_syscall aSyscall[] = {
    { "close", (syscall_ptr)close, 0 },
    { "read", (syscall_ptr)read, 0 },
    { "write", (syscall_ptr)write, 0 },
    { "access", (syscall_ptr)access, 0 },
    { "getcwd", (syscall_ptr)getcwd, 0 },
};

// A function that uses the syscall table
int dummy_function(void) {
    return aSyscall[0].pCurrent != 0;
}
