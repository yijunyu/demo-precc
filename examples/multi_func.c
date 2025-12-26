// Multi-function example demonstrating precc code splitting
// Each function can be compiled as a separate processing unit

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// Type definitions
typedef struct {
    int id;
    char name[64];
    double value;
} Record;

typedef struct Node {
    Record data;
    struct Node* next;
} Node;

// Global state
static Node* head = NULL;
static int record_count = 0;

// Helper functions
static Node* create_node(int id, const char* name, double value) {
    Node* node = (Node*)malloc(sizeof(Node));
    if (node) {
        node->data.id = id;
        strncpy(node->data.name, name, sizeof(node->data.name) - 1);
        node->data.name[sizeof(node->data.name) - 1] = '\0';
        node->data.value = value;
        node->next = NULL;
    }
    return node;
}

// List operations
void list_add(int id, const char* name, double value) {
    Node* node = create_node(id, name, value);
    if (!node) return;

    if (!head) {
        head = node;
    } else {
        Node* curr = head;
        while (curr->next) curr = curr->next;
        curr->next = node;
    }
    record_count++;
}

Record* list_find(int id) {
    Node* curr = head;
    while (curr) {
        if (curr->data.id == id) {
            return &curr->data;
        }
        curr = curr->next;
    }
    return NULL;
}

void list_remove(int id) {
    Node* prev = NULL;
    Node* curr = head;

    while (curr) {
        if (curr->data.id == id) {
            if (prev) {
                prev->next = curr->next;
            } else {
                head = curr->next;
            }
            free(curr);
            record_count--;
            return;
        }
        prev = curr;
        curr = curr->next;
    }
}

void list_print(void) {
    printf("Records (%d total):\n", record_count);
    Node* curr = head;
    while (curr) {
        printf("  [%d] %s: %.2f\n",
               curr->data.id, curr->data.name, curr->data.value);
        curr = curr->next;
    }
}

void list_free(void) {
    while (head) {
        Node* next = head->next;
        free(head);
        head = next;
    }
    record_count = 0;
}

// Main program
int main(void) {
    list_add(1, "Alpha", 10.5);
    list_add(2, "Beta", 20.3);
    list_add(3, "Gamma", 30.7);

    list_print();

    Record* r = list_find(2);
    if (r) {
        printf("\nFound: %s = %.2f\n", r->name, r->value);
    }

    list_remove(2);
    printf("\nAfter removing id=2:\n");
    list_print();

    list_free();
    return 0;
}
