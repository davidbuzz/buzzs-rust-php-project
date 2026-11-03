/*
 * Minimal stand-ins for the Zend/PHP/ext-dom declarations used by
 * php-src's ext/dom/lexbor/selectors-adapted/selectors.c, so that file can
 * be compiled unchanged outside of PHP.  The DOM-specific helpers are
 * implemented in glue.c (namespace checks call back into Rust).
 */
#ifndef PHPDOM_SHIM_H
#define PHPDOM_SHIM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <libxml/tree.h>

#define zend_always_inline inline __attribute__((always_inline))
#define ZEND_ASSERT(c) ((void) 0)
#define ZEND_UNREACHABLE() __builtin_unreachable()
#define EXPECTED(c) __builtin_expect(!!(c), 1)
#define UNEXPECTED(c) __builtin_expect(!!(c), 0)
#define ZEND_STRL(s) (s), (sizeof(s) - 1)

static inline char zend_tolower_ascii(char c)
{
	return (c >= 'A' && c <= 'Z') ? (char) (c + ('a' - 'A')) : c;
}

/* Namespace "magic tokens": small integers here, mapped in glue.c. */
#define php_dom_ns_is_html_magic_token 1
#define php_dom_ns_is_mathml_magic_token 2
#define php_dom_ns_is_svg_magic_token 3
#define php_dom_ns_is_xlink_magic_token 4
#define php_dom_ns_is_xml_magic_token 5
#define php_dom_ns_is_xmlns_magic_token 6

bool phpdom_glue_ns_is_fast(const xmlNode *node, int token);
bool phpdom_glue_ns_is_html_and_document_is_html(const xmlNode *node);
#define php_dom_ns_is_fast(node, token) phpdom_glue_ns_is_fast((const xmlNode *) (node), (token))
#define php_dom_ns_is_html_and_document_is_html(node) phpdom_glue_ns_is_html_and_document_is_html((const xmlNode *) (node))

xmlChar *php_libxml_attr_value(const xmlAttr *attr, bool *free);
bool dom_compare_value(const xmlAttr *attr, const xmlChar *value);

/* Exceptions thrown from the selector engine are recorded and picked up by Rust. */
bool phpdom_glue_exception_pending(void);
#define EG(field) phpdom_glue_exception_pending()
void php_dom_throw_error_with_message(int code, const char *message, bool strict);

#define NOT_SUPPORTED_ERR 9
#define SYNTAX_ERR 12

#endif
