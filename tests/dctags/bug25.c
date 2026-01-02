// bug25.c - K&R forward declaration uses typedef before it's defined
// Error: unknown type name 'mysize_t'
//
// Scenario: PU1 defines typedef and function using that typedef
// PU2 needs forward declaration for function from PU1, but the
// K&R forward declaration is placed BEFORE typedef definitions
//
// This reproduces SQLite error where sqlite3_uint64 is used in
// forward declaration before the typedef is available

typedef unsigned long long base_uint64;
typedef base_uint64 mysize_t;

// Function returning typedef - defined in a later PU
static mysize_t get_size(void *p);

// Main function in PU1 - needs forward declaration
void do_work(void) {
    void *data = 0;
    mysize_t sz = get_size(data);
    (void)sz;
}

// Implementation in PU2
static mysize_t get_size(void *p) {
    (void)p;
    return 42;
}
