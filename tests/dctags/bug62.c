/* Bug62: Prototype in tags but not in pu_order causes missing forward decl
 *
 * This test demonstrates the scenario where:
 * 1. A variable (handlers array) references functions like some_handler
 * 2. ctags captures both a prototype and function for some_handler
 * 3. The prototype is in tags but NOT in pu_order (no actual code)
 * 4. The bug33 fix skips K&R forward decl because prototype is in necessary
 * 5. But the prototype won't be output because it's not in pu_order
 * 6. Result: no declaration for some_handler, compilation fails
 *
 * Fix: Only skip K&R declarations if prototype is in BOTH necessary AND pu_order
 *
 * Error before fix:
 *   error: 'some_handler' undeclared here (not in a function)
 *
 * Steps to reproduce:
 * 1. gcc -E bug62.c -o bug62.i
 * 2. env PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug62.i
 * 3. gcc -c bug62.i_*.pu.c
 */

typedef int (*handler_func)(void *);

typedef struct {
    handler_func handler;
    int priority;
} Handler;

// Forward declare the handler functions (ctags captures these as prototypes)
int some_handler(void *ctx);
int other_handler(void *ctx);

// Global handler table that references functions
static Handler handlers[] = {
    { some_handler, 1 },
    { other_handler, 2 }
};

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
