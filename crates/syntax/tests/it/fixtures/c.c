#include <stdio.h>
#define MAX 10

typedef struct node {
    int value;
    struct node *next;
} node_t;

static int sum(const node_t *head) {
    int total = 0;
    while (head != NULL) {
        total += head->value * 2;
        head = head->next;
    }
    return total > MAX ? MAX : total;
}

int main(void) {
    char c = 'x';
    printf("%d\n", sum(NULL));
    return 0;
}
