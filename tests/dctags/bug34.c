// Bug34: Function embedded in struct's ctags span uses undeclared function
// Error: conflicting types for 'aggregate_context'; have 'void *(context_t *, int)'
//
// This bug occurs when:
// 1. ctags captures a struct definition with an adjacent function in its code span
// 2. The function is output during Pass 1 (types pass) as part of the struct's code
// 3. This function calls another function (e.g., sqlite3_aggregate_context)
// 4. The called function's prototype is output later in Pass 2 (forward decls)
// 5. The called function returns void*, but C implicitly declares it as int()
// 6. Later the actual prototype causes type conflict
//
// Pattern from SQLite (PU 2414):
// - struct CallCount at line 124801, ends at ~124805
// - dense_rankValueFunc at line 124824, uses sqlite3_aggregate_context
// - In PU output: struct CallCount at line 5143, dense_rankValueFunc at 5148
// - "Forward declarations" section starts at line 5308 (AFTER the function)
// - sqlite3_aggregate_context prototype appears at line 17129 (way too late)
// - Result: implicit int() declaration conflicts with void* return
//
// Root cause: The function appears in the types section (Pass 1) because it's
// captured as part of an adjacent struct's code span by ctags. The prototype
// for the called function only appears in Pass 2 (forward decls) which comes
// AFTER the function usage.
//
// Fix: Ensure that any functions appearing in struct code blocks have their
// required prototypes output BEFORE the struct, or strip functions from
// struct code spans.

typedef struct context_t context_t;
typedef struct sqlite3 sqlite3;

// Forward declare struct for context
struct context_t {
    sqlite3 *db;
    int flags;
};

// This is a non-static public API function
// Its prototype will be included via dependency, but may appear after usage
void *get_context(context_t *pCtx, int nBytes);

// In the PU, this function appears early (due to being pulled in by deps)
// but get_context's prototype appears later in the output
static void value_func(context_t *pCtx) {
    struct {
        int value;
        int count;
    } *p;

    // This call happens BEFORE the get_context prototype in PU output
    // C will implicitly declare: int get_context();
    // Later prototype says: void *get_context(context_t*, int);
    // Error: conflicting types (int vs void*)
    p = get_context(pCtx, sizeof(*p));

    if (p) {
        p->value++;
    }
}

// Another function that uses get_context (to show the pattern)
static void step_func(context_t *pCtx) {
    void *data = get_context(pCtx, 0);
    (void)data;
}

// The prototype - in the PU output, this may appear AFTER value_func
// if the dependency ordering puts value_func first
// void *get_context(context_t *pCtx, int nBytes); // already declared above

// Definition of get_context
void *get_context(context_t *pCtx, int nBytes) {
    static char buffer[1024];
    (void)pCtx;
    (void)nBytes;
    return buffer;
}

int main(void) {
    context_t ctx = {0, 0};
    value_func(&ctx);
    step_func(&ctx);
    return 0;
}
