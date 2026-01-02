// Bug30: Typedef union referencing internal glibc struct
// Error: field '__data' has incomplete type
//
// Pattern from pthread types like pthread_mutex_t:
// - A typedef union contains "struct __pthread_mutex_s __data"
// - The struct is an internal glibc type that ctags doesn't capture
// - When the typedef is output, the struct reference causes compilation to fail
//
// Fix: Skip typedef unions/structs that contain "struct __" or "union __"
// references since these are internal types we can't properly define.
//
// This is an informational test case - the bug30.c file itself compiles fine
// because we have the complete definition. The actual failure happens when
// precc extracts typedef unions from sqlite3.i that reference internal structs.

// Simulating the pattern from pthread types
struct __inner_s {
    int value;
    int count;
};

// This typedef pattern matches what caused the bug
typedef union {
    struct __inner_s __data;
    char __size[8];
    int __align;
} outer_t;

// Variable using the typedef
static outer_t my_var = { .__data = {0, 0} };

// Function using the typedef
int get_value(void) {
    return my_var.__data.value;
}

int main() {
    return get_value();
}
