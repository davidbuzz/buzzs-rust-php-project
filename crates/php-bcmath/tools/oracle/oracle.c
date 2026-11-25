/*
 * Derived from php-src ext/bcmath/bcmath.c
 *   Copyright © The PHP Group and Contributors.
 *   Author: Andi Gutmans <andi@php.net>
 * Port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
 * SPDX-License-Identifier: BSD-3-Clause
 */

/*
 * libbcmath oracle for php-bcmath's fixture tests.
 *
 * Links the unmodified php-src master libbcmath and re-implements the thin
 * glue of ext/bcmath/bcmath.c (scale handling, Number auto-scale rules).
 * Reads tab-separated requests from stdin, one per line:
 *     <op>\t<arg>\t<arg>...
 * and writes one line per request:
 *     OK\t<result>                 plain function result
 *     OK\t<value>\t<scale>         BcMath\Number result
 *     ERR\t<kind>\t<message>       kind: A<n> argument ValueError, V ValueError, Z DivisionByZeroError
 */
#include "php_bcmath.h"
#include "libbcmath/src/private.h"

zend_bcmath_globals bcmath_globals;

#define MAXARGS 6

static char *args[MAXARGS];
static int nargs;

static void out_str(zend_string *s) { fwrite(ZSTR_VAL(s), 1, ZSTR_LEN(s), stdout); free(s); }
static void ok_num(bc_num n, size_t scale) { fputs("OK\t", stdout); out_str(bc_num2str_ex(n, scale)); fputc('\n', stdout); }
static void ok_number(bc_num n, size_t scale) { fputs("OK\t", stdout); out_str(bc_num2str_ex(n, scale)); printf("\t%zu\n", scale); }
static void err(const char *kind, const char *msg) { printf("ERR\t%s\t%s\n", kind, msg); }

static bool parse(bc_num *n, const char *s, size_t *full_scale)
{
	*n = NULL;
	return bc_str2num(n, s, s + strlen(s), 0, full_scale, true);
}

static zend_enum_RoundingMode parse_mode(const char *s)
{
	static const char *names[] = {"HalfAwayFromZero", "HalfTowardsZero", "HalfEven", "HalfOdd", "TowardsZero", "AwayFromZero", "NegativeInfinity", "PositiveInfinity"};
	for (int i = 0; i < 8; i++) {
		if (strcmp(s, names[i]) == 0) return (zend_enum_RoundingMode) (i + 1);
	}
	fprintf(stderr, "bad mode %s\n", s);
	exit(2);
}

static void pow_err(bc_raise_status status, int arg_num)
{
	switch (status) {
		case BC_RAISE_STATUS_DIVIDE_BY_ZERO: err("Z", "Negative power of zero"); break;
		default:
			if (arg_num == 0) err("V", "exponent is too large, the number of digits overflowed");
			else { char k[8]; sprintf(k, "A%d", arg_num); err(k, "exponent is too large, the number of digits overflowed"); }
	}
}

/* Mirrors php-bcmath's raise_alloc_bytes(): reports what PHP could only
 * answer with a memory-limit fatal error instead of trying to compute it. */
static bool raise_too_large(bc_num base, long exponent)
{
	unsigned long long exp = exponent < 0 ? -(unsigned long long) exponent : (unsigned long long) exponent;
	size_t base_len = base->n_len + base->n_scale;
	if (exp == 0 || bc_is_zero(base)) return false;
	if (base->n_len > SIZE_MAX / exp || base->n_scale > SIZE_MAX / exp) return false; /* overflow error first */
	unsigned __int128 full = (unsigned __int128) base->n_len * exp + (unsigned __int128) base->n_scale * exp;
	if (full > SIZE_MAX) return false; /* overflow error first */
	if (base_len <= 8 && full <= 16) return false;
	size_t lz = 0;
	while (lz < base_len && base->n_value[lz] == 0) lz++;
	unsigned __int128 bytes = (unsigned __int128) ((base_len - lz + 7) / 8) * exp * 24;
	if (bytes > 256u * 1024 * 1024) { err("M", "OOM"); return true; }
	return false;
}

/* ---- plain functions ---- */
static void fn_binary(const char *op)
{
	bc_num a, b, r = NULL;
	size_t scale = strtoul(args[2], NULL, 10);
	if (!parse(&a, args[0], NULL)) { err("A1", "is not well-formed"); return; }
	if (!parse(&b, args[1], NULL)) { err("A2", "is not well-formed"); return; }
	if (!strcmp(op, "add")) { r = bc_add(a, b, scale); ok_num(r, scale); }
	else if (!strcmp(op, "sub")) { r = bc_sub(a, b, scale); ok_num(r, scale); }
	else if (!strcmp(op, "mul")) { r = bc_multiply(a, b, scale); ok_num(r, scale); }
	else if (!strcmp(op, "div")) {
		bc_init_num(&r);
		if (!bc_divide(a, b, &r, scale)) err("Z", "Division by zero"); else ok_num(r, scale);
	} else if (!strcmp(op, "mod")) {
		bc_init_num(&r);
		if (!bc_modulo(a, b, &r, scale)) err("Z", "Modulo by zero"); else ok_num(r, scale);
	} else if (!strcmp(op, "divmod")) {
		bc_num q = NULL;
		if (!bc_divmod(a, b, &q, &r, scale)) { err("Z", "Division by zero"); return; }
		fputs("OK\t", stdout); out_str(bc_num2str_ex(q, 0)); fputc('\t', stdout); out_str(bc_num2str_ex(r, scale)); fputc('\n', stdout);
	} else if (!strcmp(op, "pow")) {
		if (b->n_scale != 0) { err("A2", "cannot have a fractional part"); return; }
		long e = bc_num2long(b);
		if (e == 0 && (b->n_len > 1 || b->n_value[0] != 0)) { err("A2", "is too large"); return; }
		bc_init_num(&r);
		if (raise_too_large(a, e)) return;
		bc_raise_status st = bc_raise(a, e, &r, scale);
		if (st != BC_RAISE_STATUS_OK) { pow_err(st, 2); return; }
		ok_num(r, scale);
	} else if (!strcmp(op, "comp")) {
		bc_num x = NULL, y = NULL;
		if (!bc_str2num(&x, args[0], args[0] + strlen(args[0]), scale, NULL, false)) { err("A1", "is not well-formed"); return; }
		if (!bc_str2num(&y, args[1], args[1] + strlen(args[1]), scale, NULL, false)) { err("A2", "is not well-formed"); return; }
		printf("OK\t%d\n", (int) bc_compare(x, y, scale));
	}
}

static void fn_powmod(void)
{
	bc_num a, b, c, r;
	size_t scale = strtoul(args[3], NULL, 10);
	if (!parse(&a, args[0], NULL)) { err("A1", "is not well-formed"); return; }
	if (!parse(&b, args[1], NULL)) { err("A2", "is not well-formed"); return; }
	if (!parse(&c, args[2], NULL)) { err("A3", "is not well-formed"); return; }
	bc_init_num(&r);
	switch (bc_raisemod(a, b, c, &r, scale)) {
		case BASE_HAS_FRACTIONAL: err("A1", "cannot have a fractional part"); return;
		case EXPO_HAS_FRACTIONAL: err("A2", "cannot have a fractional part"); return;
		case EXPO_IS_NEGATIVE: err("A2", "must be greater than or equal to 0"); return;
		case MOD_HAS_FRACTIONAL: err("A3", "cannot have a fractional part"); return;
		case MOD_IS_ZERO: err("Z", "Modulo by zero"); return;
		case OK: ok_num(r, scale); return;
	}
}

static void fn_sqrt(void)
{
	bc_num r;
	size_t scale = strtoul(args[1], NULL, 10);
	if (!parse(&r, args[0], NULL)) { err("A1", "is not well-formed"); return; }
	if (bc_sqrt(&r, scale)) ok_num(r, scale); else err("A1", "must be greater than or equal to 0");
}

static void fn_floor_ceil(bool is_floor)
{
	bc_num a;
	if (!parse(&a, args[0], NULL)) { err("A1", "is not well-formed"); return; }
	ok_num(bc_floor_or_ceil(a, is_floor), 0);
}

static void fn_round(void)
{
	bc_num a, r;
	zend_long precision = strtoll(args[1], NULL, 10);
	if (!parse(&a, args[0], NULL)) { err("A1", "is not well-formed"); return; }
	bc_init_num(&r);
	size_t scale = bc_round(a, precision, parse_mode(args[2]), &r);
	ok_num(r, scale);
}

/* ---- BcMath\Number methods (receiver parsed from args[0]) ---- */
#define CHECK_RET_SCALE_OVERFLOW(scale, origin_scale) (scale > INT_MAX)

static bool num_arg(bc_num *n, size_t *fs, const char *s, int argn)
{
	char k[8];
	sprintf(k, "A%d", argn);
	if (!parse(n, s, fs)) { err(k, "is not well-formed"); return false; }
	if (*fs > INT_MAX) { err(k, "must be between 0 and 2147483647"); return false; }
	return true;
}

static void number_calc(const char *op)
{
	bc_num n1, n2, ret = NULL;
	size_t s1, s2;
	parse(&n1, args[0], &s1);
	if (!num_arg(&n2, &s2, args[1], 1)) return;
	bool auto_scale = strcmp(args[2], "-") == 0;
	size_t scale = auto_scale ? 0 : strtoul(args[2], NULL, 10);
	if (!strcmp(op, "nadd") || !strcmp(op, "nsub")) {
		if (auto_scale) scale = MAX(s1, s2);
		ret = op[1] == 'a' ? bc_add(n1, n2, scale) : bc_sub(n1, n2, scale);
		ret->n_scale = MIN(scale, ret->n_scale);
		if (bc_is_zero(ret)) ret->n_sign = PLUS;
		bc_rm_trailing_zeros(ret);
	} else if (!strcmp(op, "nmul")) {
		if (auto_scale) {
			scale = s1 + s2;
			if (CHECK_RET_SCALE_OVERFLOW(scale, s1)) { err("V", "scale of the result is too large"); return; }
		}
		ret = bc_multiply(n1, n2, scale);
		ret->n_scale = MIN(scale, ret->n_scale);
		if (bc_is_zero(ret)) ret->n_sign = PLUS;
		bc_rm_trailing_zeros(ret);
	} else if (!strcmp(op, "ndiv")) {
		if (auto_scale) scale = s1 + BC_MATH_NUMBER_EXPAND_SCALE;
		if (!bc_divide(n1, n2, &ret, scale)) { err("Z", "Division by zero"); return; }
		bc_rm_trailing_zeros(ret);
		if (auto_scale) {
			size_t diff = scale - ret->n_scale;
			scale -= diff > BC_MATH_NUMBER_EXPAND_SCALE ? BC_MATH_NUMBER_EXPAND_SCALE : diff;
		}
	} else if (!strcmp(op, "nmod")) {
		if (auto_scale) scale = MAX(s1, s2);
		if (!bc_modulo(n1, n2, &ret, scale)) { err("Z", "Modulo by zero"); return; }
		bc_rm_trailing_zeros(ret);
	} else if (!strcmp(op, "ndivmod")) {
		bc_num q = NULL;
		if (auto_scale) scale = MAX(s1, s2);
		if (!bc_divmod(n1, n2, &q, &ret, scale)) { err("Z", "Division by zero"); return; }
		bc_rm_trailing_zeros(q);
		bc_rm_trailing_zeros(ret);
		fputs("OK\t", stdout); out_str(bc_num2str_ex(q, 0)); fputs("\t0\t", stdout);
		out_str(bc_num2str_ex(ret, scale)); printf("\t%zu\n", scale);
		return;
	} else if (!strcmp(op, "npow")) {
		if (n2->n_scale != 0) { err("A1", "exponent cannot have a fractional part"); return; }
		long exponent = bc_num2long(n2);
		bool scale_expand = false;
		if (auto_scale) {
			if (exponent > 0) {
				scale = s1 * exponent;
				if (scale > INT_MAX || scale < s1) { err("V", "scale of the result is too large"); return; }
			} else if (exponent < 0) {
				scale = s1 + BC_MATH_NUMBER_EXPAND_SCALE;
				scale_expand = true;
			} else {
				scale = 0;
			}
		}
		if (exponent == 0 && (n2->n_len > 1 || n2->n_value[0] != 0)) { err("A1", "exponent is too large"); return; }
		if (raise_too_large(n1, exponent)) return;
		bc_raise_status st = bc_raise(n1, exponent, &ret, scale);
		if (st != BC_RAISE_STATUS_OK) { pow_err(st, 1); return; }
		bc_rm_trailing_zeros(ret);
		if (scale_expand) {
			size_t diff = scale - ret->n_scale;
			scale -= diff > BC_MATH_NUMBER_EXPAND_SCALE ? BC_MATH_NUMBER_EXPAND_SCALE : diff;
		}
	} else if (!strcmp(op, "ncomp")) {
		if (auto_scale) scale = MAX(n1->n_scale, n2->n_scale);
		printf("OK\t%d\n", (int) bc_compare(n1, n2, scale));
		return;
	}
	ok_number(ret, scale);
}

static void number_powmod(void)
{
	bc_num n1, e, m, ret = NULL;
	size_t s1, se, sm;
	parse(&n1, args[0], &s1);
	if (!num_arg(&e, &se, args[1], 1)) return;
	if (!num_arg(&m, &sm, args[2], 2)) return;
	size_t scale = strcmp(args[3], "-") == 0 ? 0 : strtoul(args[3], NULL, 10);
	switch (bc_raisemod(n1, e, m, &ret, scale)) {
		case BASE_HAS_FRACTIONAL: err("V", "Base number cannot have a fractional part"); return;
		case EXPO_HAS_FRACTIONAL: err("A1", "cannot have a fractional part"); return;
		case EXPO_IS_NEGATIVE: err("A1", "must be greater than or equal to 0"); return;
		case MOD_HAS_FRACTIONAL: err("A2", "cannot have a fractional part"); return;
		case MOD_IS_ZERO: err("Z", "Modulo by zero"); return;
		case OK: break;
	}
	bc_rm_trailing_zeros(ret);
	ok_number(ret, scale);
}

static void number_sqrt(void)
{
	bc_num n1;
	size_t s1;
	parse(&n1, args[0], &s1);
	bool auto_scale = strcmp(args[1], "-") == 0;
	size_t scale = auto_scale ? s1 + BC_MATH_NUMBER_EXPAND_SCALE : strtoul(args[1], NULL, 10);
	bc_num ret = bc_copy_num(n1);
	if (!bc_sqrt(&ret, scale)) { err("V", "Base number must be greater than or equal to 0"); return; }
	ret->n_scale = MIN(scale, ret->n_scale);
	bc_rm_trailing_zeros(ret);
	if (auto_scale) {
		size_t diff = scale - ret->n_scale;
		scale -= diff > BC_MATH_NUMBER_EXPAND_SCALE ? BC_MATH_NUMBER_EXPAND_SCALE : diff;
	}
	ok_number(ret, scale);
}

static void number_round(void)
{
	bc_num n1, ret = NULL;
	size_t s1;
	parse(&n1, args[0], &s1);
	size_t scale = bc_round(n1, strtoll(args[1], NULL, 10), parse_mode(args[2]), &ret);
	bc_rm_trailing_zeros(ret);
	ok_number(ret, scale);
}

static void number_floor_ceil(bool is_floor)
{
	bc_num n1;
	size_t s1;
	parse(&n1, args[0], &s1);
	ok_number(bc_floor_or_ceil(n1, is_floor), 0);
}

static void number_new(void)
{
	bc_num n1;
	size_t s1;
	if (!num_arg(&n1, &s1, args[0], 1)) return;
	ok_number(n1, s1);
}

int main(void)
{
	static char line[1 << 20];
	bc_init_numbers();
	while (fgets(line, sizeof line, stdin)) {
		size_t len = strlen(line);
		if (len && line[len - 1] == '\n') line[--len] = 0;
		char *op = line;
		nargs = 0;
		char *p = strchr(line, '\t');
		while (p && nargs < MAXARGS) {
			*p = 0;
			args[nargs++] = p + 1;
			p = strchr(p + 1, '\t');
		}
		if (!strcmp(op, "add") || !strcmp(op, "sub") || !strcmp(op, "mul") || !strcmp(op, "div") || !strcmp(op, "mod")
			|| !strcmp(op, "divmod") || !strcmp(op, "pow") || !strcmp(op, "comp")) fn_binary(op);
		else if (!strcmp(op, "powmod")) fn_powmod();
		else if (!strcmp(op, "sqrt")) fn_sqrt();
		else if (!strcmp(op, "floor")) fn_floor_ceil(true);
		else if (!strcmp(op, "ceil")) fn_floor_ceil(false);
		else if (!strcmp(op, "round")) fn_round();
		else if (!strcmp(op, "npowmod")) number_powmod();
		else if (!strcmp(op, "nsqrt")) number_sqrt();
		else if (!strcmp(op, "nround")) number_round();
		else if (!strcmp(op, "nfloor")) number_floor_ceil(true);
		else if (!strcmp(op, "nceil")) number_floor_ceil(false);
		else if (!strcmp(op, "nnew")) number_new();
		else if (op[0] == 'n') number_calc(op);
		else { fprintf(stderr, "unknown op %s\n", op); return 2; }
		fflush(stdout);
	}
	return 0;
}
