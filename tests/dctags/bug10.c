/* Bug10: Test case for struct with system types and custom types */
#include <stdlib.h>
#include <stdio.h>
#include <sys/time.h>

typedef unsigned int VOS_UINT32;
typedef unsigned char VOS_UINT8;
typedef long long VOS_INT64;

/* Mock function for test */
static void c_MSG_LTLM_ADD_CA_REQ(VOS_UINT8 *buf, VOS_UINT32 len, int a, int b) {
    (void)buf; (void)len; (void)a; (void)b;
}

int main(int argc, char **argv) {
    VOS_UINT32 len;
    VOS_UINT8* buffer = malloc(1000000);
    struct timeval tv;
    gettimeofday(&tv, 0);
    VOS_INT64 time = (VOS_INT64)tv.tv_sec * 1000 + tv.tv_usec / 1000;
    c_MSG_LTLM_ADD_CA_REQ(buffer, len, atoi("10000"), atoi("1"));
    gettimeofday(&tv, 0);
    time = (VOS_INT64)tv.tv_sec * 1000 + tv.tv_usec / 1000 - time;
    printf("%lld\n", time);
    free(buffer);
    return 0;
}
