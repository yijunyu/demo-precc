/* Bug 19: Function returning pointer used with -> but implicitly declared as int
 *
 * NOTE: This test case is INFORMATIONAL - it documents the SQLite error pattern
 * but may not trigger the bug because precc correctly extracts function prototypes.
 *
 * The SQLite bug pattern:
 * - sqlite3VdbeGetOp(p,addr)->opcode = newOpcode;
 * - When sqlite3VdbeGetOp isn't properly declared, compiler assumes int return
 * - Using -> on int causes: "invalid type argument of '->' (have 'int')"
 *
 * Error: invalid type argument of '->' (have 'int')
 *
 * SQLite failures: 6 out of 382 failures
 *
 * See actual failure: sqlite3.i_990.pu.c
 */

typedef struct {
    int opcode;
    int p1;
    int p2;
} VdbeOp;

typedef struct {
    VdbeOp *ops;
    int nOp;
} Vdbe;

// The actual function definition
VdbeOp *getVdbeOp(Vdbe *p, int addr) {
    return &p->ops[addr];
}

// Function that uses getVdbeOp with -> access
void modifyOpcode(Vdbe *p, int addr, int newOpcode) {
    getVdbeOp(p, addr)->opcode = newOpcode;
}

void modifyP1(Vdbe *p, int addr, int val) {
    getVdbeOp(p, addr)->p1 = val;
}
