// Minimal test case from /tmp/precc_exp_20251210_212419_9355ac7/vim_amalg.i_2071.pu.c
// Error at line 9485
// Error: charset.c:9485:52: error: ‘ex_breaklist’ undeclared here (not in a function); did you mean ‘breaklist’?
// Compile: gcc -E bug54.c -o bug54.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug54.i

    USEPOPUP_NONE,
    USEPOPUP_NORMAL,
    USEPOPUP_HIDDEN
} use_popup_T;



typedef enum {
    ESTACK_NONE,
    ESTACK_SFILE,
    ESTACK_STACK,
    ESTACK_SCRIPT,
} estack_arg_T;



typedef enum {
    KEYPROTOCOL_NONE,
    KEYPROTOCOL_MOK2,
    KEYPROTOCOL_KITTY,
    KEYPROTOCOL_FAIL
} keyprot_T;



typedef enum {
    FCERR_NONE,
    FCERR_UNKNOWN,
    FCERR_TOOMANY,
    FCERR_TOOFEW,
    FCERR_SCRIPT,
    FCERR_DICT,
    FCERR_OTHER,
    FCERR_DELETED,
    FCERR_NOTMETHOD,
    FCERR_FAILED,
} funcerror_T;





typedef enum {
    CPT_ABBR,
    CPT_KIND,
    CPT_MENU,
    CPT_INFO,
    CPT_COUNT,
} cpitem_T;

typedef char *(*opt_did_set_cb_T)(optset_T *args);

typedef int (*opt_expand_cb_T)(optexpand_T *args, int *numMatches, char_u ***matches);

typedef enum {
    ADDR_LINES,
    ADDR_WINDOWS,
    ADDR_ARGUMENTS,
    ADDR_LOADED_BUFFERS,
    ADDR_BUFFERS,
    ADDR_TABS,
    ADDR_TABS_RELATIVE,
    ADDR_QUICKFIX_VALID,
    ADDR_QUICKFIX,
    ADDR_UNSIGNED,
    ADDR_OTHER,
    ADDR_NONE
} cmd_addr_T;



