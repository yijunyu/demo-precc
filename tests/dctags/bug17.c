/* Bug 17: No-split mode removes extern declarations needed by static initializers
 * Expected: extern declarations should be preserved when referenced
 */

// External error message declarations (normally from errors.h)
extern char e_insufficient_arguments[];
extern char e_too_many_arguments[];

// Function that uses the extern
void some_function() {
    char *msg = e_insufficient_arguments;
}

// Static initializer that references the extern - THIS BREAKS
static char *error_ptr = e_insufficient_arguments;

void another_function() {
    char *msg = e_too_many_arguments;
}
