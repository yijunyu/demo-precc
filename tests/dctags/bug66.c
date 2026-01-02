// Bug66: glibc ctype functions with pointer-to-pointer return type
//
// Problem: glibc's ctype.h defines internal functions that return
// pointer-to-pointer types like `const unsigned short int **`.
// The EXTERN_FUNC_RE regex required whitespace between the return type
// and function name, but `int **__ctype_b_loc` has no space.
// This caused isdigit/isalpha macros to fail with:
//   error: invalid type argument of unary '*' (have 'int')
//
// Fix:
// 1. Updated EXTERN_FUNC_RE to handle `type **funcname` patterns
// 2. Added __ctype_b_loc, __ctype_tolower_loc, __ctype_toupper_loc
//    to stdlib_prototypes table with correct return types
//
// Compile: gcc -E bug66.c -o bug66.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug66.i
// Test: for f in bug66.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done

#include <ctype.h>

// Function using isdigit macro (expands to __ctype_b_loc)
int check_digit(int c)
{
    return isdigit(c);
}

// Function using isalpha macro
int check_alpha(int c)
{
    return isalpha(c);
}

// Function using toupper (expands to __ctype_toupper_loc)
int to_upper(int c)
{
    return toupper(c);
}

// Main caller
void test_ctype(void)
{
    check_digit('5');
    check_alpha('a');
    to_upper('x');
}
