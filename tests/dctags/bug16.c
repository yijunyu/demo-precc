/* Bug 16: No-split mode was only including the last function's body
 * Expected: Both ex_winsize and ex_wincmd should have their function bodies
 */
typedef struct {
    char *arg;
    int cmdidx;
} exarg_T;

// Forward declarations  
static void ex_winsize(exarg_T *eap);
static void ex_wincmd(exarg_T *eap);

// First function
    static void
ex_winsize(exarg_T *eap)
{
    int w = 1, h = 2;
}

// Second function - was missing its body before fix
    static void
ex_wincmd(exarg_T *eap)
{
    int xchar = '\000';
    char *p;
}
