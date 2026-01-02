// Minimal test case from /tmp/precc_exp_20251210_212550_9355ac7/vim_amalg.i_1862.pu.c
// Error at line 8455
// Error: tests/vim/src/version.h:8455:52: error: ‘ex_breaklist’ undeclared here (not in a function); did you mean ‘breaklist’?
// Compile: gcc -E bug55.c -o bug55.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug55.i








    colnr_T w_virtcol_first_char;
    int w_wrow, w_wcol;
    int w_lines_valid;
    wline_T *w_lines;


    garray_T w_folds;
    char w_fold_manual;

    char w_foldinvalid;



    int w_nrwidth;
    int w_redr_type;
    int w_upd_rows;

    linenr_T w_redraw_top;
    linenr_T w_redraw_bot;
    int w_redr_status;


    pos_T w_ru_cursor;
    colnr_T w_ru_virtcol;
    linenr_T w_ru_topline;
    linenr_T w_ru_line_count;

    int w_ru_topfill;

    char w_ru_empty;

    int w_alt_fnum;

    alist_T *w_alist;
    int w_arg_idx;

    int w_arg_idx_invalid;

    char_u *w_localdir;

    char_u *w_prevdir;

    vimmenu_T *w_winbar;
    winbar_item_T *w_winbar_items;
    int w_winbar_height;
    winopt_T w_onebuf_opt;
    winopt_T w_allbuf_opt;




    int *w_p_cc_cols;
    char_u w_p_culopt_flags;



    int w_briopt_min;
    int w_briopt_shift;
    int w_briopt_sbr;
    int w_briopt_list;
    int w_briopt_vcol;


    long w_scbind_pos;
