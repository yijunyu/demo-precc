// Bug71: Static function pointer variables not captured by ctags
//
// Root cause: ctags doesn't capture static function pointer variable declarations
// with complex syntax like:
//   static char_u *((*set_opt_callback_func)(expand_T *, int));
//
// This pattern declares a static variable named set_opt_callback_func which is
// a pointer to a function that takes (expand_T *, int) and returns char_u *.
//
// The fix extracts these declarations from the preprocessed file using a regex
// and includes them in PUs that reference the variable.
//
// Validation:
// gcc -E bug71.c -o bug71.i
// PASSTHROUGH_THRESHOLD=0 SPLIT=1 target/release/precc bug71.i
// for f in bug71.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done
// # All files should compile without errors

// Simple type definitions for test
typedef unsigned char char_u;
typedef struct { int dummy; } expand_T;

// Bug71 pattern: Static function pointer variable with complex syntax
// ctags doesn't capture this declaration
static char_u *((*my_callback_func)(expand_T *, int));

// Helper function that matches the function pointer signature
static char_u *actual_callback(expand_T *xp, int idx) {
    (void)xp;
    (void)idx;
    return (char_u *)"result";
}

// Function that uses the static function pointer variable
static void setup_callback(char_u *(*func)(expand_T *, int)) {
    // This references my_callback_func which ctags didn't capture
    my_callback_func = func;
}

// Function that invokes the callback
static char_u *invoke_callback(expand_T *xp, int idx) {
    if (my_callback_func != (void *)0) {
        return my_callback_func(xp, idx);
    }
    return (char_u *)"default";
}

int main(void) {
    expand_T xp = {0};
    setup_callback(actual_callback);
    char_u *result = invoke_callback(&xp, 0);
    (void)result;
    return 0;
}
