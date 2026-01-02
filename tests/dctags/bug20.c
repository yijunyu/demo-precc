/* Bug 20: Struct declared only in function parameter list not visible outside
 *
 * NOTE: This test case is INFORMATIONAL - it documents the SQLite error pattern.
 *
 * The SQLite bug pattern:
 * - A struct is first declared in a function parameter list
 * - The struct definition appears later
 * - When split, if the struct definition isn't included but a usage is, error occurs
 *
 * Error: invalid use of undefined type 'const struct ExprList_item'
 *
 * SQLite failures: 1 out of 382 failures
 */

// The actual struct definition
struct ExprList_item {
    int id;
    char *name;
};

// Forward declaration that properly uses the struct
int process_item(const struct ExprList_item *item);

typedef struct {
    int nExpr;
    struct ExprList_item *items;
} ExprList;

int process_item(const struct ExprList_item *item) {
    return item->id;
}

int count_items(ExprList *list) {
    return list->nExpr;
}
