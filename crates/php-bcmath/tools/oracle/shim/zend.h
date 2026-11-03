/* Minimal Zend shim so libbcmath (php-src master) can be compiled standalone
 * as an oracle for php-bcmath. Not part of the Rust build. */
#ifndef SHIM_ZEND_H
#define SHIM_ZEND_H
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <assert.h>

#define SIZEOF_SIZE_T 8
typedef int64_t zend_long;
typedef uint64_t zend_ulong;
#define Z_L(i) INT64_C(i)
#define ZEND_LONG_MIN INT64_MIN
#define ZEND_LONG_MAX INT64_MAX
#define LONG_MIN_DIGITS "9223372036854775808"
#define ZEND_ASSERT(c) assert(c)
#define EXPECTED(c) __builtin_expect(!!(c), 1)
#define UNEXPECTED(c) __builtin_expect(!!(c), 0)
#define ZEND_UNREACHABLE() abort()
#define ZEND_BYTES_SWAP64(x) __builtin_bswap64(x)
#define ZEND_BYTES_SWAP32(x) __builtin_bswap32(x)
#define ZEND_MM_ALIGNMENT 8
#define PHP_HAVE_BUILTIN_CTZL 1

static inline void *shim_alloc(size_t n) { void *p = malloc(n ? n : 1); if (!p) { fprintf(stderr, "oom\n"); exit(3); } return p; }
#define emalloc(n) shim_alloc(n)
#define safe_emalloc(a, b, c) shim_alloc((a) * (b) + (c))
#define efree(p) free(p)
#define pemalloc(n, p) shim_alloc(n)
#define pefree(x, p) free(x)
static inline size_t zend_safe_address_guarded(size_t nmemb, size_t size, size_t offset) { return nmemb * size + offset; }

typedef struct { size_t len; char val[1]; } zend_string;
#define ZSTR_VAL(s) ((s)->val)
#define ZSTR_LEN(s) ((s)->len)
static inline zend_string *zend_string_alloc(size_t len, int persistent) {
	(void) persistent;
	zend_string *s = shim_alloc(sizeof(zend_string) + len + 1);
	s->len = len;
	return s;
}

typedef enum {
	ZEND_ENUM_RoundingMode_HalfAwayFromZero = 1,
	ZEND_ENUM_RoundingMode_HalfTowardsZero,
	ZEND_ENUM_RoundingMode_HalfEven,
	ZEND_ENUM_RoundingMode_HalfOdd,
	ZEND_ENUM_RoundingMode_TowardsZero,
	ZEND_ENUM_RoundingMode_AwayFromZero,
	ZEND_ENUM_RoundingMode_NegativeInfinity,
	ZEND_ENUM_RoundingMode_PositiveInfinity,
} zend_enum_RoundingMode;
#endif
