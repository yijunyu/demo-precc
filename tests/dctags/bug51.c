// Minimal test case from /tmp/precc_exp_20251210_191344_e0f840b/vim_amalg.i_1862.pu.c
// Error at line 9426
// Error: buffer.c:9426:52: error: ‘ex_breaklist’ undeclared here (not in a function); did you mean ‘breaklist’?
// Compile: gcc -E bug51.c -o bug51.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug51.i

    int vmode;


    short_u origin_row;
    short_u origin_start_col;
    short_u origin_end_col;
    short_u word_start_col;
    short_u word_end_col;


    short_u min_col;
    short_u max_col;
    short_u min_row;
    short_u max_row;


    pos_T prev;
    short_u state;
    short_u mode;


    Atom sel_atom;
} Clipboard_T;

typedef struct stat stat_T;



typedef struct soundcb_S soundcb_T;

typedef enum {
    ASSERT_EQUAL,
    ASSERT_NOTEQUAL,
    ASSERT_MATCH,
    ASSERT_NOTMATCH,
    ASSERT_FAILS,
    ASSERT_OTHER
} assert_type_T;



typedef enum {
    PASTE_INSERT,
    PASTE_CMDLINE,
    PASTE_EX,
    PASTE_ONE_CHAR
} paste_mode_T;



typedef enum {
    FLUSH_MINIMAL,
    FLUSH_TYPEAHEAD,
    FLUSH_INPUT
} flush_buffers_T;



typedef enum {
    USEPOPUP_NONE,
    USEPOPUP_NORMAL,
    USEPOPUP_HIDDEN
} use_popup_T;



typedef enum {
    ESTACK_NONE,
    ESTACK_SFILE,
    ESTACK_STACK,
    ESTACK_SCRIPT,
