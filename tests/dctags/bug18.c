/* Bug 18: Variadic function forward declaration conflicts with definition
 *
 * NOTE: This test case is INFORMATIONAL - it documents the SQLite error pattern
 * but may not actually trigger the bug because precc correctly extracts the
 * variadic prototype from the preprocessed source.
 *
 * The SQLite bug pattern:
 * - Line 919: int sqlite3_vtab_config(sqlite3*, int op, ...);  // prototype
 * - Line 97392: sqlite3_vtab_config,                           // func ptr usage
 * - Line 114635: int sqlite3_vtab_config(sqlite3 *db, int op, ...){  // definition
 *
 * When the prototype (line 919) is not in the same PU as line 97392,
 * precc generates "int sqlite3_vtab_config();" K&R stub.
 * Later in the same PU, the real variadic definition appears, causing conflict.
 *
 * Error: conflicting types for 'sqlite3_vtab_config'; have 'int(sqlite3 *, int, ...)'
 * note: previous declaration of 'sqlite3_vtab_config' with type 'int()'
 *
 * SQLite failures: 368 out of 382 failures
 *
 * See actual failure: sqlite3.i_2484.pu.c lines 206 vs 15152
 */

#include <stdarg.h>

typedef struct {
    int value;
} config_t;

// The variadic function definition
int some_variadic_func(config_t *cfg, int op, ...) {
    va_list args;
    va_start(args, op);
    int result = va_arg(args, int);
    va_end(args);
    return result;
}

// Function that uses some_variadic_func by pointer reference
void use_func_pointer(void) {
    void *ptr = (void*)some_variadic_func;
    (void)ptr;
}

typedef int (*variadic_handler)(config_t *, int, ...);
static variadic_handler handlers[] = {
    some_variadic_func
};

void call_via_table(config_t *cfg) {
    handlers[0](cfg, 1, 42);
}
