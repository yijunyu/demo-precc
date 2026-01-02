// Minimal test case from /tmp/precc_exp_20251211_094512_b8b4d0b-dirty/vim_amalg.i_1848.pu.c
// Error at line 9251
// Error: eval.c:9251:52: error: ‘ex_breaklist’ undeclared here (not in a function); did you mean ‘breaklist’?
// Compile: gcc -E bug58.c -o bug58.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug58.i


} spellvars_T;






typedef struct
{
    int key;
    string_T value;
} keyvalue_T;







struct cellsize {
    int cs_xpixel;
    int cs_ypixel;
};





typedef struct vwl_connection_S vwl_connection_T;

typedef struct vwl_seat_S vwl_seat_T;




typedef struct vwl_data_offer_S vwl_data_offer_T;

typedef struct vwl_data_source_S vwl_data_source_T;

typedef struct vwl_data_device_S vwl_data_device_T;

typedef struct vwl_data_device_manager_S vwl_data_device_manager_T;


typedef struct vwl_data_device_listener_S vwl_data_device_listener_T;

typedef struct vwl_data_source_listener_S vwl_data_source_listener_T;

typedef struct vwl_data_offer_listener_S vwl_data_offer_listener_T;



typedef enum {
    WAYLAND_SELECTION_NONE = 0,
    WAYLAND_SELECTION_REGULAR = 1 << 0,
    WAYLAND_SELECTION_PRIMARY = 1 << 1,
} wayland_selection_T;

typedef struct s_mmfile {
 char *ptr;
 long size;
} mmfile_t;


typedef struct s_mmbuffer {
 char *ptr;
 long size;
} mmbuffer_t;


