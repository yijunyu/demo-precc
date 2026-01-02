// Bug69: K&R declarations using void* break when return value is dereferenced
//
// Root cause: For functions with non-basic return types (like char_u*), the
// generate_minimal_forward_decl function was falling back to void* to avoid
// transitive typedef dependency issues. However, void* cannot be dereferenced
// in C - code like "*ml_get(...)" would fail with "void value not ignored".
//
// This test validates that K&R declarations use the actual typedef when it's
// available in the PU, rather than always falling back to void*.
//
// Example from vim's buffer.c:
//   ml_get() returns char_u* (typedef unsigned char char_u)
//   Code pattern: *ml_get((linenr_T)1) == '\0'
//   Old K&R: void *ml_get(); -> "void value not ignored as it ought to be"
//   New K&R: char_u * ml_get(); -> compiles correctly
//
// The fix checks if the return type typedef is in the available_types set
// before falling back to void*. If the typedef is available, the actual
// return type is used in the K&R declaration.
//
// Validation:
// gcc -E bug69.c -o bug69.i
// PASSTHROUGH_THRESHOLD=0 SPLIT=1 target/release/precc bug69.i
// for f in bug69.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done
// # All files should compile without errors

typedef unsigned char char_u;
typedef long linenr_T;

// Function returning typedef'd pointer type
// This function is defined in this file but will be converted to a K&R declaration
// when it appears in a different PU's necessary set.
char_u *get_line_content(linenr_T lnum) {
    static char_u line[] = "Hello";
    return line;
}

// This function dereferences the result of get_line_content
// Without bug69 fix, it would get void *get_line_content();
// which fails when dereferenced with *
int check_first_char(linenr_T lnum) {
    // This dereference would fail if get_line_content returned void*
    if (*get_line_content(lnum) == 'H')
        return 1;
    return 0;
}

// Another function that chains calls
char_u get_char_at(linenr_T lnum) {
    return *get_line_content(lnum);
}

// Main function for standalone testing
int main(void) {
    return check_first_char(1);
}
