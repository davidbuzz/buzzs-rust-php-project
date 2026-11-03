#ifndef PHP_BCMATH_H
#define PHP_BCMATH_H
#include "zend.h"
#include "libbcmath/src/bcmath.h"
#define BC_ARENA_SIZE 256
typedef struct {
	bc_num _zero_;
	bc_num _one_;
	bc_num _two_;
	int bc_precision;
	char *arena;
	size_t arena_offset;
} zend_bcmath_globals;
extern zend_bcmath_globals bcmath_globals;
#define BCG(v) (bcmath_globals.v)
#define BC_MATH_NUMBER_EXPAND_SCALE 10
#endif
