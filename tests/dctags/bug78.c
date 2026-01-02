// Bug78: Extern variables using custom types not in necessary set
// should be filtered out to avoid "unknown type" errors
//
// Problem: When precc outputs extern variable declarations like
// "extern clipmethod_T clipmethod;", the typedef "clipmethod_T" may
// not be in the necessary set, causing "unknown type" errors.
//
// Fix: Filter out extern variable declarations that use types not
// available in the necessary set.

// Custom type that simulates clipmethod_T (a vim typedef enum)
typedef enum {
    METHOD_NONE,
    METHOD_ONE,
    METHOD_TWO,
} custom_type_T;

// Define the extern variable (normally in another TU)
custom_type_T custom_var = METHOD_NONE;

// Another type that won't be in necessary set for some PUs
typedef struct {
    int value;
} other_type_T;

other_type_T other_var = {0};

// Simple function that uses the variable
int get_method(void) {
    return (int)custom_var;
}

// Function that uses other_var
int get_value(void) {
    return other_var.value;
}

// Main function (required for PU)
int main(void) {
    return get_method() + get_value();
}
