// Bug73: Forward declarations for always_inline functions must have __attribute__((always_inline)) stripped
// Otherwise, gcc errors: "inlining failed in call to 'always_inline' 'func': function body not available"
// The always_inline attribute requires the function body to be available for inlining,
// but forward declarations have no body.

// This function has always_inline attribute
static __attribute__((always_inline)) inline int allocateSpace(int *pPage, int nByte, int *pIdx) {
    *pIdx = nByte;
    return 0;
}

// This function calls allocateSpace - when in a PU where allocateSpace is output as a declaration,
// the declaration should have always_inline stripped to avoid "inlining failed" error
static int insertCell(int *page, int sz, int *idx) {
    return allocateSpace(page, sz, idx);
}
