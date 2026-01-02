// Bug31: Duplicate extern function declarations
// Error: conflicting types for 'some_func'
//
// This bug occurred when the same extern function declarations were
// written to the output file multiple times. The duplicate declarations
// caused "conflicting types" errors, especially when functions have
// special parameter types like "struct stat64" that declare the struct
// inside the parameter list.
//
// Pattern from SQLite (PU 758):
// - Two code paths in precc (print_necessary_units and
//   print_necessary_units_chunked) both wrote extern declarations
// - When both paths executed for the same PU, the declarations appeared twice
// - For functions with inline struct declarations in parameters, the second
//   declaration refers to a different struct type, causing type conflicts
//
// Fix: Added externs_written flag to track if extern declarations were
// already written, preventing duplicate output.
//
// Affected SQLite PUs: 758, 810, 929, 1092, 1094, 1205, 1228, 1241, 1244, 1248, 1551, 1552

#include <stdio.h>

// Simulate a function with an inline struct declaration in parameters
// When duplicated, "struct inline_s" in each declaration refers to
// a different type, causing "conflicting types" error
struct inline_s {
    int field1;
    int field2;
};

extern int special_func(struct inline_s *param);

// A simple function that uses the extern function
int use_extern(void) {
    struct inline_s data = {1, 2};
    return special_func(&data);
}

// Definition of the extern function
int special_func(struct inline_s *param) {
    return param->field1 + param->field2;
}

int main() {
    return use_extern();
}
