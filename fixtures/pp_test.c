#include <stdio.h>
#include <stdlib.h>

(%include "be_included.c"%)
(%include_string "be_included.c"%)
(%include_bytes "be_included.c"%)

A

(%function_macro AA() 123%)
(%function_macro CASCADE() AA()%)
(%function_macro CASCADE2() CASCADE%)

CASCADE2()()

(%function_macro f(x, y) (x + y)%)
(%object_macro ma f(1, 0)%)
(%object_macro mb f(2, ma)%)
(%object_macro mc f(3, mb)%)

mc

int main(int argc, char** argv) {
        printf("Hello World");
        return 0;
}