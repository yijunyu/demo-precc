// Bug77: Function calls ending with ); incorrectly detected as prototypes
//
// Root cause: The embedded_prototypes detection logic incorrectly identified
// function CALLS (lines ending with ");") as prototype declarations.
//
// False positive patterns:
// 1. "return sqlite3_bind_int64(p, i, (i64)iValue);" - return statement with function call
// 2. "return sqlite3_value_double(p->apArg[p->nUsed++]);" - return statement with method access
// 3. "zType, lineno, 20+sqlite3_sourceid());" - expression with function call in args
// 4. "sqlite3_result_error(p->pCtx, "...", -1);" - function call without return type prefix
//
// These were being added to embedded_prototypes, causing K&R forward declarations
// to be skipped, but no actual prototype was output, resulting in "undeclared" errors.
//
// Fix: Enhanced the embedded_prototypes detection to skip:
// 1. Lines where function name has no return type prefix (no space/separator before it)
// 2. Lines where word before function name is a keyword (return, if, while, etc.)
// 3. Lines where character before function name is an operator (+, -, /, etc.)
// 4. Lines with = or -> or . before function name (assignments, method calls)
//
// Compile: gcc -E bug77.c -o bug77.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug77.i
// Verify: for f in bug77.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done

struct context;
typedef struct context sqlite3_context;
typedef struct value sqlite3_value;
typedef long long sqlite3_int64;
typedef void (*destructor_type)(void*);

// Prototypes that should be captured
void sqlite3_result_error(sqlite3_context*, const char*, int);
int sqlite3_bind_int64(void*, int, sqlite3_int64);
double sqlite3_value_double(sqlite3_value*);
const char *sqlite3_sourceid(void);

// Function with return statement containing function call - NOT a prototype
int sqlite3_bind_int(void *p, int i, int iValue) {
    return sqlite3_bind_int64(p, i, (sqlite3_int64)iValue);
}

// Function using function in expression - NOT a prototype
double getDoubleArg(struct { sqlite3_value **apArg; int nUsed; } *p) {
    return sqlite3_value_double(p->apArg[p->nUsed++]);
}

// Function call as argument to another function - NOT a prototype
void sqlite3ReportError(const char *zType, int lineno) {
    // This line was incorrectly detected as prototype:
    // zType, lineno, 20+sqlite3_sourceid());
}

// Struct with function pointers using these functions
struct api_routines {
    void (*result_error)(sqlite3_context*, const char*, int);
    int (*bind_int64)(void*, int, sqlite3_int64);
    double (*value_double)(sqlite3_value*);
    const char *(*sourceid)(void);
};

static const struct api_routines sqlite3Apis = {
    sqlite3_result_error,
    sqlite3_bind_int64,
    sqlite3_value_double,
    sqlite3_sourceid,
};

// Function that uses the API struct
void test_apis(sqlite3_context *ctx) {
    sqlite3Apis.result_error(ctx, "test error", -1);
}
