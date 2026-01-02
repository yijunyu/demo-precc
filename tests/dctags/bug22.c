/*
 * bug22.c - K&R forward declaration return type mismatch (INFORMATIONAL)
 *
 * Problem: When a function returns a struct pointer, but the K&R forward
 * declaration uses void*, the compiler sees a type conflict.
 *
 * Root cause: In complex codebases like SQLite, typedef ordering can cause
 * precc to generate void* K&R stubs when the actual return type isn't yet
 * available at the point where the stub is generated.
 *
 * In SQLite: sqlite3VdbeGetOp returns VdbeOp*, but K&R stub says void*
 *
 * Status: INFORMATIONAL - This simplified test case compiles successfully
 * because precc correctly includes the typedef dependencies. The bug only
 * manifests in SQLite due to complex typedef ordering across ~260K LOC.
 *
 * The error pattern when it occurs:
 *   error: unknown type name 'VdbeOp'
 *   error: request for member 'opcode' in something not a structure or union
 */

// Forward declare types
typedef struct Vdbe Vdbe;
typedef struct VdbeOp VdbeOp;

struct Vdbe {
    int dummy;
};

struct VdbeOp {
    int opcode;
};

// Forward declare (simulates what SQLite has)
static VdbeOp *sqlite3VdbeGetOp(Vdbe *p, int addr);

// This function uses sqlite3VdbeGetOp and VdbeOp.
// When split by precc, if VdbeOp typedef isn't included in this PU,
// it will fail with "unknown type name 'VdbeOp'"
static void useVdbeOp(Vdbe *v) {
    VdbeOp *op = sqlite3VdbeGetOp(v, 0);
    op->opcode = 99;
}

// Function that returns VdbeOp*
static VdbeOp *sqlite3VdbeGetOp(Vdbe *p, int addr) {
    static VdbeOp op = {80};
    (void)p;
    (void)addr;
    return &op;
}

int main() {
    Vdbe v;
    useVdbeOp(&v);
    return 0;
}
