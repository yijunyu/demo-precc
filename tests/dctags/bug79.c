// Bug79: Character and string literal content replaced with placeholder
//
// The lexer was replacing character/string literal content with placeholder
// characters (CHAR_SYMBOL and STRING_SYMBOL) during tokenization. This caused:
//   case '+': to become case 'c':
// Which leads to "duplicate case value" errors when multiple cases exist.
//
// The fix: Store literal content in a buffer during tokenization and output
// the actual content when emitting the symbol.
//
// Expected: All switch cases with different char literals work correctly
// Was failing: "duplicate case value" errors

static int process_char(int c) {
    switch (c) {
        case '+':
            return 1;
        case '-':
            return 2;
        case '*':
            return 3;
        case '/':
            return 4;
        case '\\':
            return 5;
        case '\'':
            return 6;
        case '"':
            return 7;
        case '\n':
            return 8;
        case '\t':
            return 9;
        case '\0':
            return 10;
        default:
            return 0;
    }
}

static const char *test_string = "Hello, World!";
static const char *special_chars = "\\n\\t\\'\\\"";
