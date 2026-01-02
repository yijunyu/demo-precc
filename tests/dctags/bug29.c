// Bug29: Missing typedef dependency in struct field types
// Error: unknown type name 'ValueType' and 'BaseType'
// The struct uses a typedef in its fields but the typedef dependency is not tracked

// This simulates the chain: sqlite_int64 -> sqlite3_int64 -> sqlite3StatValueType
typedef long long BaseType;
typedef BaseType ValueType;

// This struct uses ValueType as a field type
typedef struct StatType StatType;
static struct StatType {
    ValueType nowValue[10];
    ValueType mxValue[10];
} stats = { {0,}, {0,} };

// Function that uses the struct
static BaseType getValue(int op) {
    return stats.nowValue[op];
}

int main() {
    return getValue(0);
}
