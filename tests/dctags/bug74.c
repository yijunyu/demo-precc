// Bug74: Function prototypes with function pointer parameters were incorrectly
// skipped when scanning for embedded prototypes. Two issues were fixed:
//
// 1. The check `trimmed.contains("(*")` would skip prototypes like:
//    static int sqlite3VdbeMemSetStr(Mem*, const char*, i64, u8, void(*)(void*));
//    because "(*" appears in the function pointer parameter, not just pure
//    function pointer declarations.
//
//    Fix: Only check for "(*" BEFORE the first opening parenthesis.
//
// 2. Function name extraction used `rfind('(')` which finds the inner `(` in
//    function pointer parameters like `void(*)(void*)`, not the main function's
//    parameter list opening. Also, `__attribute__((...))` before function names
//    would cause the first `(` to be the attribute's parenthesis.
//
//    Fix: Strip __attribute__ prefixes before searching, and use `find('(')`
//    instead of `rfind('(')` to get the function's parameter list.
//
// Compile: gcc -E bug74.c -o bug74.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug74.i
// Verify: gcc -c bug74.i_*.pu.c (should compile without "conflicting types" error)

typedef unsigned char u8;
typedef long long i64;

struct Mem { int dummy; };
typedef struct Mem Mem;

struct Btree { int dummy; };
typedef struct Btree Btree;

// This prototype has a function pointer PARAMETER - should NOT be skipped
static int sqlite3VdbeMemSetStr(Mem*, const char*, i64, u8, void(*)(void*));

// This prototype has __attribute__ before function name - should NOT be skipped
static void __attribute__((noinline)) btreeLockCarefully(Btree *p);

// This is a pure function pointer declaration - SHOULD be skipped
void (*callback)(int);

// Main function that calls functions with function pointer parameters
char *sqlite3Utf16to8(void) {
    Mem m;
    Btree b;
    sqlite3VdbeMemSetStr(&m, "test", 4, 1, ((void(*)(void*))0));
    btreeLockCarefully(&b);
    return 0;
}
