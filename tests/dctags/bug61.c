// Bug61: K&R-style function with return type on separate line
//
// Problem: Vim uses K&R-style function definitions where the return type
// is on a separate line from the function name:
//     void
// func_name(void)
//
// The K&R forward declaration generator only looked at the line containing
// the function name, missing the return type on the previous line.
// This caused `int func_name();` forward declarations for functions that
// return `void`, leading to "conflicting types" errors.
//
// Fix: Track the previous non-preprocessor line and use it as the
// signature_prefix when the function name line has no prefix.
//
// Compile: gcc -E bug61.c -o bug61.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug61.i
// Test: for f in bug61.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done

// K&R style: return type on separate line
    void
limit_screen_size(void)
{
    // Function body
}

    static void
term_is_builtin(void)
{
    // Static function with void return
}

// Normal style for comparison
int normal_func(void)
{
    return 0;
}

// Function that calls the K&R style functions
void test_caller(void)
{
    limit_screen_size();
    term_is_builtin();
    normal_func();
}
