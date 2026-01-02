// Bug60: Anonymous enum enumerators not captured by ctags
//
// Problem: Enumerators from anonymous enums (e.g., KS_SPECIAL, KS_XON)
// were not being added to the tags map, so functions depending on
// them couldn't resolve their dependencies.
//
// Vim uses this pattern extensively in term.c:
//   enum { KS_SPECIAL = 128, KS_XON = 129, ... };
//
// Fix:
// 1. Changed ctags c-kinds to include enumerators (e) and enum names (g)
// 2. Add enumerators to tags map for dependency resolution
// 3. Map enumerators to parent enum (both anonymous and named)
// 4. Fix function_map to use full pu_key format "function:name:file"
// 5. Fix primary function counter to only count functions/variables in split mode
//
// Compile: gcc -E bug60.c -o bug60.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug60.i
// Test: for f in bug60.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done

// Named enum - should work
enum Color { RED, GREEN, BLUE };

// Anonymous enum - was failing before fix
enum { KS_SPECIAL = 128, KS_XON = 129, KS_XOFF = 130 };

// Another anonymous enum
enum { FLAG_A = 1, FLAG_B = 2, FLAG_C = 4 };

// Function using anonymous enum constants
void test_anonymous_enum() {
    int x = KS_SPECIAL;
    int y = KS_XON;
    int z = FLAG_A | FLAG_B;
}

// Function using named enum constants
void test_named_enum() {
    enum Color c = RED;
    int r = GREEN;
}

// Function using both
void test_mixed() {
    enum Color c = BLUE;
    int flags = KS_XOFF | FLAG_C;
}
