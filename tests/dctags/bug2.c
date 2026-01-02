enum eState {
 DRCTV_NONE,
 DRCTV_DEFINE,
 DRCTV_HASH,
 DRCTV_IF,
 DRCTV_PRAGMA,
 DRCTV_UNDEF
};
typedef struct sCppState {
 int ungetch, ungetch2;
 boolean resolveRequired;
 boolean hasAtLiteralStrings;
 struct sDirective {
  enum eState state;
  boolean accept;
  vString * name;
  unsigned int nestLevel;
  conditionalInfo ifdef [MaxCppNestingLevel];
 } directive;
} cppState;

