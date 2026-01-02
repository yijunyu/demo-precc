// Bug28: Missing glibc internal typedef (__uint16_t)
// Error: unknown type name '__uint16_t'
// The __uint16_t type is defined in glibc headers but not captured by ctags

typedef unsigned short __uint16_t;

static __inline __uint16_t
__bswap_16 (__uint16_t __bsx)
{
  return __builtin_bswap16 (__bsx);
}

// Main function that uses __bswap_16
int main() {
    __uint16_t x = 0x1234;
    __uint16_t y = __bswap_16(x);
    return y;
}
