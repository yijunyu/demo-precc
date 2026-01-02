/*
 * Test case for bug15: convert_function_to_declaration creates invalid syntax
 *
 * Issue: When a function signature spans multiple lines and doesn't have a body ({}),
 * the convert_function_to_declaration function was adding a semicolon on a new line,
 * creating invalid C syntax like:
 *
 *     static char_u *
 *     replace_makeprg(exarg_T *eap, char_u *p, char_u **cmdlinep)
 *     ;
 *
 * Expected: The semicolon should be on the same line as the closing parenthesis:
 *
 *     static char_u *
 *     replace_makeprg(exarg_T *eap, char_u *p, char_u **cmdlinep);
 */

typedef struct {
    int dummy;
} exarg_T;

typedef unsigned char char_u;

// Function with multi-line signature that needs to be converted to declaration
static char_u *
replace_makeprg(exarg_T *eap, char_u *p, char_u **cmdlinep)
{
    return p;
}

// Another multi-line function
static void
skip_grep_pat(exarg_T *eap)
{
    int x = 1;
}

// Single-line function for comparison
static int simple_function(int x) {
    return x + 1;
}
