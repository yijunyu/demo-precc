// Minimal test case from /tmp/precc_exp_20251229_113824_1abefc7-dirty/sqlite3.i_1304.pu.c
// Error at line 123
// Error: /tmp/precc_exp_20251229_113824_1abefc7-dirty/sqlite3.i_1304.pu.c:123:21: error: invalid use of undefined type ‘const struct ExprList_item’
// Compile: gcc -E bug81.c -o bug81.i
// Split: PASSTHROUGH_THRESHOLD=0 SPLIT=1 ../../target/release/precc bug81.i

typedef unsigned int __kernel_uid32_t;
typedef unsigned int __kernel_gid32_t;
typedef long long __kernel_loff_t;
typedef long long __kernel_time64_t;
typedef int __kernel_timer_t;
typedef int __kernel_clockid_t;
typedef unsigned short __kernel_uid16_t;
typedef unsigned short __kernel_gid16_t;
typedef unsigned __poll_t;
// Forward declarations for functions defined elsewhere

static int sqlite3IsRowid(const char*);



static const unsigned char sqlite3UpperToLower[] = {

      0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17,
     18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35,
     36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,
     54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 97, 98, 99,100,101,102,103,
    104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120,121,
    122, 91, 92, 93, 94, 95, 96, 97, 98, 99,100,101,102,103,104,105,106,107,
    108,109,110,111,112,113,114,115,116,117,118,119,120,121,122,123,124,125,
    126,127,128,129,130,131,132,133,134,135,136,137,138,139,140,141,142,143,
    144,145,146,147,148,149,150,151,152,153,154,155,156,157,158,159,160,161,
    162,163,164,165,166,167,168,169,170,171,172,173,174,175,176,177,178,179,
    180,181,182,183,184,185,186,187,188,189,190,191,192,193,194,195,196,197,
    198,199,200,201,202,203,204,205,206,207,208,209,210,211,212,213,214,215,
    216,217,218,219,220,221,222,223,224,225,226,227,228,229,230,231,232,233,
    234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,250,251,
    252,253,254,255,

   1, 0, 0, 1, 1, 0,
   0, 1, 0, 1, 0, 1,
   1, 0, 1, 0, 0, 1
};
static int sqlite3StrICmp(const char *zLeft, const char *zRight);
int sqlite3_strnicmp(const char *zLeft, const char *zRight, int N);


static int sqlite3MatchEName(
  const struct ExprList_item *pItem,
  const char *zCol,
  const char *zTab,
  const char *zDb,
  int *pbRowid
){
  int n;
  const char *zSpan;
  int eEName = pItem->fg.eEName;
  if( eEName!=2 && (eEName!=3 || (pbRowid==0)) ){
    return 0;
  }
  

 ((void) (0))

                                    ;
  zSpan = pItem->zEName;
  for(n=0; (zSpan[n]) && zSpan[n]!='.'; n++){}
  if( zDb && (sqlite3_strnicmp(zSpan, zDb, n)!=0 || zDb[n]!=0) ){
    return 0;
  }
  zSpan += n+1;
  for(n=0; (zSpan[n]) && zSpan[n]!='.'; n++){}
  if( zTab && (sqlite3_strnicmp(zSpan, zTab, n)!=0 || zTab[n]!=0) ){
    return 0;
  }
  zSpan += n+1;
  if( zCol ){
