/* Bug 21: Function used as pointer value but not declared
 *
 * This test SUCCESSFULLY reproduces the precc bug where functions used
 * as pointer values (for comparison or in initializers) aren't declared.
 *
 * Error: 'some_handler' undeclared (first use in this function)
 *
 * SQLite failures: 7 out of 382 failures (various function pointer issues)
 *
 * Steps to reproduce:
 * 1. gcc -E bug21.c -o bug21.i
 * 2. env PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug21.i
 * 3. gcc -c bug21.i_1.pu.c  # Shows: 'some_handler' undeclared
 */

typedef int (*handler_func)(void *);

typedef struct {
    handler_func handler;
    int priority;
} Handler;

// Forward declare the handler functions
int some_handler(void *ctx);
int other_handler(void *ctx);

// Global handler table that references functions
static Handler handlers[] = {
    { some_handler, 1 },
    { other_handler, 2 }
};

// Function that compares function pointers
int is_some_handler(handler_func f) {
    // Comparison requires function to be declared
    return f == some_handler;
}

// Actual function definitions
int some_handler(void *ctx) {
    (void)ctx;
    return 1;
}

int other_handler(void *ctx) {
    (void)ctx;
    return 2;
}

int call_first_handler(void *ctx) {
    return handlers[0].handler(ctx);
}
