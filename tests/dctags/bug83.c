// Bug83: Nested struct names not interned, causing parameter references to fail
// Error: invalid use of undefined type 'const struct ExprList_item'
// Root cause: Nested structs are in nested_struct_to_parent but NOT in name_interner
// Fix: Intern nested struct names from nested_struct_to_parent.keys()
// Compile: gcc -E bug83.c -o bug83.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug83.i

// Parent struct containing a nested struct
struct ExprList {
    int nExpr;
    struct ExprList_item {
        int iConstExprReg;
        int fg_done;
        int fg_eEName;
        char *zEName;
    } a[1];
};

// Function that takes a pointer to the nested struct type
// This failed before bug83 fix because "ExprList_item" wasn't in name_interner
static int matchNestedItem(
    const struct ExprList_item *pItem,
    const char *zName
) {
    if (pItem->fg_eEName != 2) {
        return 0;
    }
    return pItem->zEName[0] == zName[0];
}

// Another function using the nested struct
static void processItem(struct ExprList_item *item) {
    item->fg_done = 1;
}

// Main function that uses the parent struct
int useExprList(struct ExprList *pList) {
    int i;
    for (i = 0; i < pList->nExpr; i++) {
        if (matchNestedItem(&pList->a[i], "test")) {
            processItem(&pList->a[i]);
            return 1;
        }
    }
    return 0;
}
