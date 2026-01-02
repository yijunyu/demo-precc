// Bug36: Typedef using struct needs struct to be defined first
// The typedef "jmp_buf" uses "struct __jmp_buf_tag" which requires
// the struct definition (or at least forward declaration) to appear first

typedef long int __jmp_buf[8];

struct __jmp_buf_tag {
    __jmp_buf __jmpbuf;
    int __mask_was_saved;
    unsigned long __saved_mask;
};

typedef struct __jmp_buf_tag jmp_buf[1];

extern jmp_buf x_jump_env;

void test_jmp(void) {
    (void)x_jump_env;
}
