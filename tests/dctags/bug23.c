/*
 * bug23.c - Struct declared inside parameter list not visible (INFORMATIONAL)
 *
 * Problem: When precc splits the code, if the struct definition ends up in
 * a different PU than the function that uses it, the PU without the struct
 * definition gets "storage size isn't known" errors.
 *
 * Root cause: In complex codebases like SQLite, struct dependencies may not
 * be fully captured when the struct is only used in function bodies.
 *
 * In SQLite: struct ExprList_item is used in sqlite3MatchEName parameter
 *
 * Status: INFORMATIONAL - This simplified test case compiles successfully
 * because precc correctly includes the struct dependencies. The bug only
 * manifests in SQLite due to complex dependency chains across ~260K LOC.
 *
 * The error pattern when it occurs:
 *   error: storage size of 'item' isn't known
 */

// The struct definition
struct ExprList_item {
    struct {
        int eEName;
    } fg;
    const char *zEName;
};

// Forward declaration
static int sqlite3MatchEName(
    const struct ExprList_item *pItem,
    const char *zCol
);

// Function that uses ExprList_item in the body
// When split, if struct isn't included, gets "storage size isn't known"
static int useExprListItem(void) {
    struct ExprList_item item;
    item.fg.eEName = 1;
    item.zEName = "test";
    return sqlite3MatchEName(&item, "col");
}

// Function that uses the struct in parameter
static int sqlite3MatchEName(
    const struct ExprList_item *pItem,
    const char *zCol
) {
    if (!pItem) return 0;
    int eEName = pItem->fg.eEName;
    const char *zSpan = pItem->zEName;
    (void)zCol;
    (void)zSpan;
    return eEName;
}

int main() {
    return useExprListItem();
}
