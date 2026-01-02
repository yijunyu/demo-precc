// Bug57: Struct truncated when false-positive prototype is inside struct body
// ctags incorrectly emits "prototype:char_u" for struct member "char_u *(cp_text[CPT_COUNT]);"
// which causes the struct definition to be truncated

typedef enum {
    CPT_ABBR,
    CPT_KIND,
    CPT_MENU,
    CPT_INFO,
    CPT_COUNT,
} cpitem_T;

typedef int typval_T;
typedef char char_u;

typedef struct compl_S compl_T;
struct compl_S
{
    compl_T *cp_next;
    compl_T *cp_prev;
    char_u *(cp_text[CPT_COUNT]);
    typval_T cp_user_data;  // Should be included
    char_u *cp_fname;        // Should be included
    int cp_flags;            // Should be included
};

static compl_T *compl_first_match = 0;

int use_match(void) {
    return compl_first_match->cp_flags;
}
