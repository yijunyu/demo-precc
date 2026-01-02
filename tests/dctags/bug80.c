// Bug80: String-initialized arrays with inferred size become incomplete types
//
// When a static array is declared with [] (size inferred from initializer)
// and uses a string literal initializer, the forward declaration conversion
// was stripping the initializer, leaving an incomplete type:
//   static const char_u base64_table[] ;   // Error: incomplete type
//
// This causes sizeof(base64_table) to fail with "incomplete type" error.
//
// The fix: detect string-initialized arrays (not just brace-initialized)
// and preserve the full definition instead of converting to declaration.
//
// Expected: sizeof(base64_table) works correctly
// Was failing: "invalid application of 'sizeof' to incomplete type"

typedef unsigned char char_u;

// String-initialized array with inferred size
static const char_u base64_table[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static char_u base64_dec_table[256];

static void init_base64_dec_table(void) {
    // Use sizeof(base64_table) - this would fail with incomplete type
    for (int i = 0; i < sizeof(base64_table) - 1; i++)
        base64_dec_table[(char_u)base64_table[i]] = (char_u)i;
}
