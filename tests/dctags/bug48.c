// Minimal test case from /tmp/precc_exp_20251210_183643_56c0f5f/vim_amalg.i_1000.pu.c
// Error at line 12545
// Error: /tmp/precc_exp_20251210_183643_56c0f5f/vim_amalg.i_1000.pu.c:12545:9: error: ‘wl_callback_interface’ undeclared (first use in this function)
// Compile: gcc -E bug48.c -o bug48.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug48.i

 int32_t h;
};

typedef int (*wl_dispatcher_func_t)(const void *, void *, uint32_t,
        const struct wl_message *,
        union wl_argument *);

typedef void (*wl_log_func_t)(const char *, va_list) __attribute__((__format__(__printf__, 1, 0)));







struct wl_display_listener {
 void (*error)(void *data,
        struct wl_display *wl_display,
        void *object_id,
        uint32_t code,
        const char *message);
 void (*delete_id)(void *data,
     struct wl_display *wl_display,
     uint32_t id);
};

static inline void
wl_display_set_user_data(struct wl_display *wl_display, void *user_data)
{
 wl_proxy_set_user_data((struct wl_proxy *) wl_display, user_data);
}


static inline void *
wl_display_get_user_data(struct wl_display *wl_display)
{
 return wl_proxy_get_user_data((struct wl_proxy *) wl_display);
}

static inline uint32_t
wl_display_get_version(struct wl_display *wl_display)
{
 return wl_proxy_get_version((struct wl_proxy *) wl_display);
}
static inline struct wl_callback *
wl_display_sync(struct wl_display *wl_display)
{
 struct wl_proxy *callback;

 callback = wl_proxy_marshal_flags((struct wl_proxy *) wl_display,
    0, &wl_callback_interface, wl_proxy_get_version((struct wl_proxy *) wl_display), 0, ((void *)0));

 return (struct wl_callback *) callback;
}
static inline struct wl_registry *
wl_display_get_registry(struct wl_display *wl_display)
{
 struct wl_proxy *registry;

 registry = wl_proxy_marshal_flags((struct wl_proxy *) wl_display,
    1, &wl_registry_interface, wl_proxy_get_version((struct wl_proxy *) wl_display), 0, ((void *)0));

 return (struct wl_registry *) registry;
}





struct wl_registry_listener {
 void (*global)(void *data,
