// Bug47: Forward struct declaration ordering
// Error: 'struct wl_display' declared inside parameter list
//
// When ctags captures "struct foo;" as externvar (instead of struct),
// the forward declaration wasn't being output before structs that
// reference "struct foo *" in their members.
//
// Fix: Add Pass 0 to output forward struct/union declarations
// (externvar entries that start with "struct " or "union " and end with ";")
// before Pass 1 outputs the actual struct definitions.

struct wl_display;

struct wl_display_listener {
    void (*error)(void *data, struct wl_display *wl_display);
};

void test_func(struct wl_display *d) {
    (void)d;
}
