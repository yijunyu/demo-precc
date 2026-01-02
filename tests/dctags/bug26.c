// bug26.c - K&R forward declaration uses typedef that has transitive dependencies
//
// When a function like `static i64 get_value()` needs a K&R forward declaration,
// the typedef `i64` may depend on other typedefs (e.g., `typedef sqlite_int64 i64;`).
// If we use the actual type name in the K&R declaration, we need ALL transitive
// dependencies to be defined first.
//
// The safest solution is to always use basic types (int, void*) in K&R forward
// declarations, avoiding the transitive dependency problem entirely.

typedef long long base_int64;
typedef base_int64 myint64;
typedef myint64 i64;

// This function's return type has transitive dependencies - NO prototype here
// The function is defined later, so variables referencing it need forward decls
static i64 get_value(int x) {
    return (i64)x * 2;
}

// A variable that references get_value - forces K&R forward declaration in another PU
static int (*fp)(void) = (int (*)(void))get_value;

void do_work(void) {
    i64 val = get_value(42);
    (void)val;
    (void)fp;
}
