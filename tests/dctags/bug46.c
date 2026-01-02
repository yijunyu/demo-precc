// Minimal test case from /tmp/precc_exp_20251210_150316_4965c60/sqlite3.i_1000.pu.c
// Error at line 1350
// Error: /tmp/precc_exp_20251210_150316_4965c60/sqlite3.i_1000.pu.c:1350:3: error: unknown type name ‘__pthread_list_t’
// Compile: gcc -E bug46.c -o bug46.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug46.i



typedef __fsblkcnt64_t fsblkcnt_t;




typedef __fsfilcnt64_t fsfilcnt_t;






typedef __blkcnt64_t blkcnt64_t;

typedef __fsblkcnt64_t fsblkcnt64_t;

typedef __fsfilcnt64_t fsfilcnt64_t;







typedef union
{
  __extension__ unsigned long long int __value64;
  struct
  {
    unsigned int __low;
    unsigned int __high;
  } __value32;
} __atomic_wide_counter;

struct __pthread_mutex_s
{
  int __lock;
  unsigned int __count;
  int __owner;

  unsigned int __nusers;



  int __kind;

  short __spins;
  short __elision;
  __pthread_list_t __list;
};

struct __pthread_rwlock_arch_t
{
  unsigned int __readers;
  unsigned int __writers;
  unsigned int __wrphase_futex;
  unsigned int __writers_futex;
  unsigned int __pad3;
  unsigned int __pad4;

  int __cur_writer;
  int __shared;
  signed char __rwelision;




  unsigned char __pad1[7];

