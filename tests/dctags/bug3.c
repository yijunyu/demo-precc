/* Bug3: Test case for function with custom type and missing header */
#include <string.h>
typedef int boolean;

static boolean charIsIn (char ch, const char* list)
{
 return (strchr (list, ch) != 0);
}

