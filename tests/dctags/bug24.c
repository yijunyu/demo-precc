/*
 * bug24.c - K&R stub returns void*, but return value is dereferenced
 *
 * Problem: When K&R forward declaration returns void* but the actual function
 * returns a struct pointer, code using the return value with -> operator
 * gets "request for member in something not a structure or union".
 *
 * Root cause: When precc generates a forward declaration for a function that
 * isn't in the current PU, it uses K&R style (no return type info). If the
 * actual return type isn't available, precc defaults to void*. Code that
 * dereferences the return value then fails.
 *
 * In SQLite: sqlite3VdbeGetLastOp(v)->opcode where stub returns void*
 *
 * Status: REPRODUCES BUG - This test case successfully reproduces the error
 * pattern seen in SQLite. After running precc, bug24.i_1.pu.c fails with:
 *   warning: dereferencing 'void *' pointer
 *   error: request for member 'opcode' in something not a structure or union
 *
 * To test:
 *   gcc -E bug24.c -o bug24.i
 *   PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug24.i
 *   gcc -c bug24.i_1.pu.c  # Should fail
 */

typedef struct Vdbe Vdbe;
typedef struct VdbeOp VdbeOp;

struct Vdbe {
    int dummy;
};

struct VdbeOp {
    int opcode;
};

// Forward declaration (simulates what SQLite has)
static VdbeOp *sqlite3VdbeGetLastOp(Vdbe *p);

// This function dereferences sqlite3VdbeGetLastOp's return value.
// When split by precc, if the proper forward declaration isn't included,
// precc generates: static void *sqlite3VdbeGetLastOp();
// Then the -> operator fails because void* can't be dereferenced.
static void setDoNotMergeFlagOnCopy(Vdbe *v) {
    if (sqlite3VdbeGetLastOp(v)->opcode == 80) {
        // do something
    }
}

// Function that returns VdbeOp*
static VdbeOp *sqlite3VdbeGetLastOp(Vdbe *p) {
    static VdbeOp op = {80};
    (void)p;
    return &op;
}

int main() {
    Vdbe v;
    setDoNotMergeFlagOnCopy(&v);
    return 0;
}
