// Bug35: Multiple typedef names on same line
// ctags only captures the first name, truncating the typedef

struct _XOC;

typedef struct _XOC *XOC, *XFontSet;

typedef struct { int x; } Point, *PointPtr;

void use_types(void) {
    XFontSet fs;
    PointPtr pp;
    (void)fs;
    (void)pp;
}
