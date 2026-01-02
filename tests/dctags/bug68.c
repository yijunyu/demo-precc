// Bug68: Garbage code fragments appear after function declarations
//
// Root cause: The brace matching in convert_function_to_declaration_with_name
// was counting '}' characters inside character literals (e.g., "if (*p == '}')")
// causing premature brace depth decrement and leaving garbage code after declarations.
//
// This test validates that functions with character literals containing braces
// are correctly converted to declarations without garbage.
//
// Example from vim's cindent.c:
//   cin_iselse() has "if (*p == '}')" which caused garbage like "else if (*s == ';"
//   to appear after its declaration.
//
// The fix adds proper parsing to skip:
// - Character literals ('x', '\n', etc.)
// - String literals ("...")
// - Line comments (//)
// - Block comments (/* ... */)
//
// Validation:
// gcc -E bug68.c -o bug68.i
// PASSTHROUGH_THRESHOLD=0 SPLIT=1 target/release/precc bug68.i
// for f in bug68.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done
// # All files should compile without errors

#include <string.h>

// Test case 1: Character literal with closing brace
static int test_brace_in_char(char *p) {
    if (*p == '}')
        p++;
    return (*p == '{');
}

// Test case 2: String literal with braces
static int test_brace_in_string(char *s) {
    if (strcmp(s, "{}") == 0)
        return 1;
    return 0;
}

// Test case 3: Mix of quotes and braces
static int test_mixed_quotes(char *p) {
    if (*p == '"') {
        p++;
        while (*p && *p != '"') {
            if (*p == '{' || *p == '}')
                p++;
            p++;
        }
    }
    return *p;
}

// Test case 4: Function that depends on test_brace_in_char
// This ensures the declaration is generated correctly
static int caller_function(char *s) {
    return test_brace_in_char(s) + test_brace_in_string(s);
}

// Main function for standalone testing
int main(void) {
    char test[] = "{}test";
    return caller_function(test);
}
