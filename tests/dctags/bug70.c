// Bug70: Struct aliases not found when inline struct defined with variable/typedef
//
// Root cause: When a struct is defined inline with a variable or typedef declaration,
// ctags creates an alias (e.g., key_name_entry -> variable:key_names_table).
// The scan_for_typedef_references function only checked struct_map which didn't
// include these aliases, so the struct definition wasn't included in the PU.
//
// This test validates that inline struct definitions are correctly included
// when referenced via their struct tag name.
//
// Example patterns:
// 1. Inline struct with static array:
//    static struct key_name_entry { ... } key_names_table[] = {...};
//    ctags creates: key_name_entry -> variable:key_names_table:file
//
// 2. Inline struct with typedef:
//    typedef struct subs_expr_S { ... } subs_expr_T;
//    ctags creates: subs_expr_S -> typedef:subs_expr_T:file
//
// The fix adds checking tags for struct aliases when not found in struct_map,
// including both variable and typedef aliases.
//
// Validation:
// gcc -E bug70.c -o bug70.i
// PASSTHROUGH_THRESHOLD=0 SPLIT=1 target/release/precc bug70.i
// for f in bug70.i_*.pu.c; do gcc -c "$f" && echo "$f: OK" || echo "$f: FAIL"; done
// # All files should compile without errors

// Test case 1: Inline struct defined with static array variable
static struct key_entry {
    int key;
    char name[32];
} key_table[] = {
    {1, "one"},
    {2, "two"},
    {3, "three"},
};

// Function that uses the struct via cast
static int compare_keys(const void *a, const void *b) {
    // This uses struct key_entry which is aliased to the variable key_table
    const struct key_entry *ka = (const struct key_entry *)a;
    const struct key_entry *kb = (const struct key_entry *)b;
    return ka->key - kb->key;
}

// Test case 2: Inline struct with typedef
typedef struct data_item_S {
    int value;
    char *label;
} data_item_T;

// Function that uses the struct tag (not the typedef)
static void process_data(void *ptr) {
    // This uses struct data_item_S which is aliased to typedef data_item_T
    struct data_item_S *item = (struct data_item_S *)ptr;
    item->value = 42;
}

// Main function for standalone testing
int main(void) {
    compare_keys(&key_table[0], &key_table[1]);
    data_item_T item = {0, "test"};
    process_data(&item);
    return 0;
}
