// Bug50: Function pointers in static arrays that are NOT captured by ctags
// but whose prototypes ARE output in the PU (embedded in other code spans)
//
// This test demonstrates the scenario where:
// 1. A variable (global_functions array) references functions like f_test
// 2. ctags does NOT capture f_test as a function/prototype tag
// 3. But f_test's prototype IS included in the PU output (as part of another tag's code span)
// 4. The fix should NOT generate a K&R declaration (int f_test();) because it would
//    conflict with the actual prototype (void f_test(...))
//
// Error before fix:
//   error: conflicting types for 'f_test'
//   note: previous declaration of 'f_test' with type 'int()'

typedef struct {
    int x;
    int y;
} typval_T;

// This prototype is adjacent to other functions and may be captured as part
// of another tag's code span, even though ctags doesn't capture it directly
    static void
f_test(typval_T *argvars, typval_T *rettv);

typedef void (*f_func_type)(typval_T *, typval_T *);

typedef struct {
    const char *name;
    f_func_type f_func;
} funcentry_T;

// This variable references f_test which ctags may not capture
static funcentry_T global_functions[] = {
    {"test", f_test},
};

    static void
f_test(typval_T *argvars, typval_T *rettv)
{
    (void)argvars;
    rettv->x = 42;
}

int main(void) {
    typval_T args, result;
    global_functions[0].f_func(&args, &result);
    return 0;
}
