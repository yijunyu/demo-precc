// Minimal test case for function returning function pointer
// Bug: ctags doesn't capture unixDlSym because of complex return type,
// causing unixDlClose's code span to include unixDlSym's body
// Compile: gcc -E bug59.c -o bug59.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug59.i

// Function that returns a function pointer - ctags doesn't parse this correctly
static void (*unixDlSym(int *NotUsed, void *p, const char*zSym))(void){
  void (*(*x)(void*,const char*))(void);
  (void)(NotUsed);
  return (*x)(p, zSym);
}

// Simple function after it - its code span gets corrupted
static void unixDlClose(int *NotUsed, void *pHandle){
  (void)(NotUsed);
}

// Struct initializer that references both functions
typedef struct {
    void* (*dlOpen)(void);
    void (*dlClose)(void*, void*);
    void (*(*dlSym)(int*, void*, const char*))(void);
} vfs_funcs;

// This uses both functions as function pointers
static vfs_funcs funcs = {
    0,
    (void(*)(void*,void*))unixDlClose,
    unixDlSym
};

// Function that uses the struct
int main(void) {
    (void)funcs;
    return 0;
}
