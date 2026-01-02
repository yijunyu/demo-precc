// Bug72: Extern declarations using project typedefs must appear AFTER Pass 1 typedefs
// Otherwise, `langType` in `extern void addKeyword(langType language);` is undefined
// because the typedef isn't output until Pass 1.

typedef int langType;
typedef int keywordId;

// This extern declaration uses langType - it must appear AFTER the typedef is defined
extern void addKeyword(const char *string, langType language, keywordId id);

void buildKeywordHash(langType language) {
    addKeyword("test", language, 1);
}
