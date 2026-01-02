// Bug32: Prototype declaration appears after function that uses it
// Error: conflicting types for 'get_aggregate_context'
//
// This bug occurs when:
// 1. A static function calls a non-static function that returns void*
// 2. The non-static function's prototype appears LATER in the output
//    than the static function's definition
// 3. Without a forward declaration, GCC assumes the function returns int
// 4. When the actual prototype (returning void*) appears later, it conflicts
//
// Pattern from SQLite (PU 2414):
// - dense_rankValueFunc (static) calls sqlite3_aggregate_context (returns void*)
// - The prototype for sqlite3_aggregate_context appears after dense_rankValueFunc
// - Implicit int declaration conflicts with the void* return type
//
// Fix: Prototypes for functions returning non-int types must be output
// BEFORE any code that calls them, not just included in the dependency list.

typedef struct context_t context_t;

// This prototype should appear BEFORE any function that uses it
// In the buggy case, the dependency system includes it but orders it AFTER
void *get_aggregate_context(context_t *ctx, int size);

// This static function calls get_aggregate_context
// If the prototype above appears AFTER this in output, we get:
// "conflicting types for 'get_aggregate_context'"
static void value_func(context_t *ctx) {
    void *p;
    // Without prototype before this, GCC assumes: int get_aggregate_context()
    p = get_aggregate_context(ctx, sizeof(int));
    if (p) {
        *(int*)p = 42;
    }
}

// Another static function that also uses get_aggregate_context
static void step_func(context_t *ctx) {
    int *counter = (int*)get_aggregate_context(ctx, sizeof(int));
    if (counter) {
        (*counter)++;
    }
}

// Definition of the non-static function
void *get_aggregate_context(context_t *ctx, int size) {
    static int storage;
    (void)ctx;
    (void)size;
    return &storage;
}

int main() {
    context_t *ctx = (context_t*)0;
    value_func(ctx);
    step_func(ctx);
    return 0;
}
