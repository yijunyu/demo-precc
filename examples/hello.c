// Simple example for precc demonstration

#include <stdio.h>

// Forward declaration
int add(int a, int b);
int multiply(int a, int b);

// Global variable
int global_counter = 0;

// Function definitions
int add(int a, int b) {
    global_counter++;
    return a + b;
}

int multiply(int a, int b) {
    global_counter++;
    return a * b;
}

void print_result(const char* op, int result) {
    printf("%s: %d\n", op, result);
}

int main() {
    int x = 5, y = 3;

    print_result("add", add(x, y));
    print_result("multiply", multiply(x, y));

    printf("Operations performed: %d\n", global_counter);

    return 0;
}
