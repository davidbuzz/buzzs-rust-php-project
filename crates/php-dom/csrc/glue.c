/*
 * C glue between php-dom (Rust) and lexbor.
 *
 * Everything here needs lexbor's struct layouts or inline functions, so it
 * lives in C; the libxml2-side logic (the lexbor -> libxml2 bridge, the
 * serializers, DOM properties) is in Rust.  Large parts are adapted from
 * php-src ext/dom (html_document.c, html5_parser.c,
 * inner_outer_html_mixin.c, parentnode/css_selectors.c), BSD-3-Clause,
 * Copyright (c) The PHP Group and Contributors, author Nora Dossche.
 */

#include "phpdom_shim.h"

#include <stdlib.h>
#include <lexbor/html/parser.h>
#include <lexbor/html/encoding.h>
#include <lexbor/html/interfaces/element.h>
#include <lexbor/html/interfaces/template_element.h>
#include <lexbor/dom/dom.h>
#include <lexbor/encoding/encoding.h>
#include <lexbor/core/array_obj.h>
#include <lexbor/core/swar.h>
#include <lexbor/css/parser.h>
#include <lexbor/css/log.h>
#include <lexbor/ns/ns.h>
#include <lexbor/tag/tag.h>
#include "selectors.h"

/* ------------------------------------------------------------------ */
/* Shim implementations used by selectors.c                            */
/* ------------------------------------------------------------------ */

typedef bool (*phpdom_ns_is_fast_fn)(const xmlNode *node, int token);
typedef bool (*phpdom_ns_html_doc_fn)(const xmlNode *node);

static phpdom_ns_is_fast_fn g_ns_is_fast = NULL;
static phpdom_ns_html_doc_fn g_ns_html_doc = NULL;

void phpdom_glue_init(phpdom_ns_is_fast_fn ns_is_fast, phpdom_ns_html_doc_fn ns_html_doc)
{
	g_ns_is_fast = ns_is_fast;
	g_ns_html_doc = ns_html_doc;
}

bool phpdom_glue_ns_is_fast(const xmlNode *node, int token)
{
	return g_ns_is_fast != NULL && g_ns_is_fast(node, token);
}

bool phpdom_glue_ns_is_html_and_document_is_html(const xmlNode *node)
{
	return g_ns_html_doc != NULL && g_ns_html_doc(node);
}

static __thread const char *g_exception_message = NULL;
static __thread int g_exception_code = 0;

bool phpdom_glue_exception_pending(void)
{
	return g_exception_message != NULL;
}

void php_dom_throw_error_with_message(int code, const char *message, bool strict)
{
	(void) strict;
	g_exception_code = code;
	g_exception_message = message;
}

/* Returns and clears the pending exception (a static string), or NULL. */
const char *phpdom_glue_take_exception(int *code)
{
	const char *message = g_exception_message;
	*code = g_exception_code;
	g_exception_message = NULL;
	g_exception_code = 0;
	return message;
}

xmlChar *php_libxml_attr_value(const xmlAttr *attr, bool *free)
{
	*free = false;
	if (attr->children == NULL) {
		return BAD_CAST "";
	}
	if (attr->children->type == XML_TEXT_NODE && attr->children->next == NULL) {
		return attr->children->content == NULL ? BAD_CAST "" : attr->children->content;
	}
	xmlChar *value = xmlNodeGetContent((const xmlNode *) attr);
	if (value == NULL) {
		return BAD_CAST "";
	}
	*free = true;
	return value;
}

bool dom_compare_value(const xmlAttr *attr, const xmlChar *value)
{
	bool should_free;
	xmlChar *attr_value = php_libxml_attr_value(attr, &should_free);
	bool result = xmlStrEqual(attr_value, value);
	if (should_free) {
		xmlFree(attr_value);
	}
	return result;
}

/* ------------------------------------------------------------------ */
/* CSS selectors (parentnode/css_selectors.c)                          */
/* ------------------------------------------------------------------ */

typedef struct {
	lxb_css_parser_t parser;
	lxb_selectors_t selectors;
	lxb_css_selector_list_t *list;
} phpdom_css_query;

typedef int (*phpdom_css_cb)(const xmlNode *node, void *ctx);

typedef struct {
	phpdom_css_cb cb;
	void *ctx;
} phpdom_css_cb_ctx;

static lxb_status_t phpdom_css_trampoline(const xmlNode *node, lxb_css_selector_specificity_t spec, void *ctx)
{
	(void) spec;
	phpdom_css_cb_ctx *c = ctx;
	return c->cb(node, c->ctx) ? LXB_STATUS_STOP : LXB_STATUS_OK;
}

void phpdom_css_free(phpdom_css_query *q)
{
	if (q == NULL) {
		return;
	}
	lxb_css_selector_list_destroy_memory(q->list);
	lxb_selectors_destroy(&q->selectors);
	(void) lxb_css_parser_destroy(&q->parser, false);
	free(q);
}

/* Compiles a selector list.  On failure returns NULL and copies the first
 * log message (if any) into `err` (`*err_len` = (size_t) -1 when there is
 * no message). */
phpdom_css_query *phpdom_css_compile(const char *selectors, size_t len, bool match_first, bool quirks,
	char *err, size_t err_cap, size_t *err_len)
{
	*err_len = (size_t) -1;
	phpdom_css_query *q = calloc(1, sizeof(*q));
	if (q == NULL) {
		return NULL;
	}
	(void) lxb_css_parser_init(&q->parser, NULL);
	(void) lxb_selectors_init(&q->selectors);
	lxb_selectors_opt_t options = match_first ? LXB_SELECTORS_OPT_MATCH_FIRST : LXB_SELECTORS_OPT_DEFAULT;
	if (quirks) {
		options |= LXB_SELECTORS_OPT_QUIRKS_MODE;
	}
	lxb_selectors_opt_set(&q->selectors, options);

	q->list = lxb_css_selectors_parse(&q->parser, (const lxb_char_t *) selectors, len);
	if (q->list == NULL) {
		size_t nr = lexbor_array_obj_length(&q->parser.log->messages);
		if (nr > 0) {
			lxb_css_log_message_t *msg = lexbor_array_obj_get(&q->parser.log->messages, 0);
			size_t n = msg->text.length < err_cap ? msg->text.length : err_cap;
			memcpy(err, msg->text.data, n);
			*err_len = n;
		}
		phpdom_css_free(q);
		return NULL;
	}
	return q;
}

int phpdom_css_find(phpdom_css_query *q, const xmlNode *root, phpdom_css_cb cb, void *ctx)
{
	phpdom_css_cb_ctx c = { cb, ctx };
	lxb_status_t status = lxb_selectors_find(&q->selectors, root, q->list, phpdom_css_trampoline, &c);
	return status == LXB_STATUS_OK || status == LXB_STATUS_STOP ? 0 : -1;
}

int phpdom_css_match_node(phpdom_css_query *q, const xmlNode *node, phpdom_css_cb cb, void *ctx)
{
	phpdom_css_cb_ctx c = { cb, ctx };
	lxb_status_t status = lxb_selectors_match_node(&q->selectors, node, q->list, phpdom_css_trampoline, &c);
	return status == LXB_STATUS_OK || status == LXB_STATUS_STOP ? 0 : -1;
}

/* ------------------------------------------------------------------ */
/* Encodings                                                           */
/* ------------------------------------------------------------------ */

const lxb_encoding_data_t *phpdom_encoding_by_name(const char *name, size_t len)
{
	return lxb_encoding_data_by_name((const lxb_char_t *) name, len);
}

const char *phpdom_encoding_name(const lxb_encoding_data_t *data)
{
	return (const char *) data->name;
}

bool phpdom_encoding_is_utf8(const lxb_encoding_data_t *data)
{
	return data->encoding == LXB_ENCODING_UTF_8;
}

typedef void (*phpdom_write_fn)(void *ctx, const char *buf, size_t len);

/* dom_common_save(): converts UTF-8 serializer output to `data`'s encoding. */
void phpdom_encode_from_utf8(const lxb_encoding_data_t *data, const char *in, size_t len, phpdom_write_fn write, void *wctx)
{
	const lxb_encoding_data_t *decoding_data = lxb_encoding_data(LXB_ENCODING_UTF_8);
	lxb_encoding_encode_t encode;
	lxb_encoding_decode_t decode;
	lxb_char_t encoding_output[4096];
	lxb_codepoint_t codepoints[4096];
	(void) lxb_encoding_encode_init(&encode, data, encoding_output, sizeof(encoding_output) / sizeof(*encoding_output));
	(void) lxb_encoding_decode_init(&decode, decoding_data, codepoints, sizeof(codepoints) / sizeof(*codepoints));
	if (data->encoding == LXB_ENCODING_UTF_8) {
		lxb_encoding_encode_replace_set(&encode, LXB_ENCODING_REPLACEMENT_BYTES, LXB_ENCODING_REPLACEMENT_SIZE);
	} else {
		lxb_encoding_encode_replace_set(&encode, (const lxb_char_t *) "?", 1);
	}
	lxb_encoding_decode_replace_set(&decode, LXB_ENCODING_REPLACEMENT_BUFFER, LXB_ENCODING_REPLACEMENT_BUFFER_LEN);

	const lxb_char_t *buf_ref = (const lxb_char_t *) in;
	const lxb_char_t *buf_end = buf_ref + len;

	if (data->encoding == LXB_ENCODING_UTF_8) {
		const lxb_char_t *last_output = buf_ref;
		while (buf_ref != buf_end) {
			const lxb_char_t *buf_ref_backup = buf_ref;
			lxb_codepoint_t codepoint = lxb_encoding_decode_utf_8_single(&decode, &buf_ref, buf_end);
			if (codepoint > LXB_ENCODING_MAX_CODEPOINT) {
				write(wctx, (const char *) last_output, buf_ref_backup - last_output);
				if (codepoint == LXB_ENCODING_DECODE_CONTINUE) {
					decode.status = LXB_STATUS_CONTINUE;
					last_output = buf_ref;
					break;
				}
				write(wctx, (const char *) LXB_ENCODING_REPLACEMENT_BYTES, LXB_ENCODING_REPLACEMENT_SIZE);
				last_output = buf_ref;
			}
		}
		if (buf_ref != last_output) {
			write(wctx, (const char *) last_output, buf_ref - last_output);
		}
	} else {
		lxb_status_t decode_status, encode_status;
		do {
			decode_status = lxb_encoding_decode_utf_8(&decode, &buf_ref, buf_end);
			const lxb_codepoint_t *codepoints_ref = codepoints;
			const lxb_codepoint_t *codepoints_end = codepoints_ref + lxb_encoding_decode_buf_used(&decode);
			do {
				encode_status = data->encode(&encode, &codepoints_ref, codepoints_end);
				write(wctx, (const char *) encoding_output, lxb_encoding_encode_buf_used(&encode));
				lxb_encoding_encode_buf_used_set(&encode, 0);
			} while (encode_status == LXB_STATUS_SMALL_BUFFER);
			lxb_encoding_decode_buf_used_set(&decode, 0);
		} while (decode_status == LXB_STATUS_SMALL_BUFFER);
	}

	(void) lxb_encoding_decode_finish(&decode);
	if (lxb_encoding_decode_buf_used(&decode)) {
		const lxb_codepoint_t *codepoints_ref = codepoints;
		(void) data->encode(&encode, &codepoints_ref, codepoints_ref + lxb_encoding_decode_buf_used(&decode));
		write(wctx, (const char *) encoding_output, lxb_encoding_encode_buf_used(&encode));
		lxb_encoding_encode_buf_used_set(&encode, 0);
	}
	(void) lxb_encoding_encode_finish(&encode);
	if (lxb_encoding_encode_buf_used(&encode)) {
		write(wctx, (const char *) encoding_output, lxb_encoding_encode_buf_used(&encode));
	}
}

/* ------------------------------------------------------------------ */
/* HTML document parsing (html_document.c)                             */
/* ------------------------------------------------------------------ */

#define DOM_FALLBACK_ENCODING_ID LXB_ENCODING_UTF_8
#define PHPDOM_CHUNK 4096

/* is_tree, code name, line, column, len (0/1 = single column). */
typedef void (*phpdom_html_error_fn)(void *app, bool is_tree, const char *code, size_t line, size_t column, size_t len);

typedef struct {
	size_t last_line;
	size_t last_column;
	size_t last_offset;
} dom_line_column_cache;

typedef struct {
	bool fast_path;
	lxb_encoding_encode_t encode;
	lxb_encoding_decode_t decode;
	const lxb_encoding_data_t *encode_data;
	const lxb_encoding_data_t *decode_data;
	lxb_char_t encoding_output[PHPDOM_CHUNK];
	lxb_codepoint_t codepoints[PHPDOM_CHUNK];
} dom_decoding_encoding_ctx;

typedef struct phpdom_html_parser {
	/* application data */
	const lxb_codepoint_t *current_input_codepoints;
	const char *current_input_characters;
	size_t current_input_length;
	size_t current_total_offset;
	dom_line_column_cache cache_tokenizer;
	bool html_no_implied;
	bool report_errors;
	phpdom_html_error_fn error_fn;
	void *error_app;

	dom_decoding_encoding_ctx dec;
	const lxb_encoding_data_t *override_encoding;
	bool determine_implicitly;
	lxb_html_document_t *document;
	lxb_html_parser_t *parser;
	size_t tokenizer_error_offset;
	size_t tree_error_offset;
	char buf[PHPDOM_CHUNK];
} phpdom_html_parser;

static const char *dom_lexbor_tokenizer_error_code_to_string(lxb_html_tokenizer_error_id_t id)
{
	switch (id) {
		case LXB_HTML_TOKENIZER_ERROR_ABCLOFEMCO: return "abrupt-closing-of-empty-comment";
		case LXB_HTML_TOKENIZER_ERROR_ABDOPUID: return "abrupt-doctype-public-identifier";
		case LXB_HTML_TOKENIZER_ERROR_ABDOSYID: return "abrupt-doctype-system-identifier";
		case LXB_HTML_TOKENIZER_ERROR_ABOFDIINNUCHRE: return "absence-of-digits-in-numeric-character-reference";
		case LXB_HTML_TOKENIZER_ERROR_CDINHTCO: return "cdata-in-html-content";
		case LXB_HTML_TOKENIZER_ERROR_CHREOUUNRA: return "character-reference-outside-unicode-range";
		case LXB_HTML_TOKENIZER_ERROR_COCHININST: return "control-character-in-input-stream";
		case LXB_HTML_TOKENIZER_ERROR_COCHRE: return "control-character-reference";
		case LXB_HTML_TOKENIZER_ERROR_ENTAWIAT: return "end-tag-with-attributes";
		case LXB_HTML_TOKENIZER_ERROR_DUAT: return "duplicate-attribute";
		case LXB_HTML_TOKENIZER_ERROR_ENTAWITRSO: return "end-tag-with-trailing-solidus";
		case LXB_HTML_TOKENIZER_ERROR_EOBETANA: return "eof-before-tag-name";
		case LXB_HTML_TOKENIZER_ERROR_EOINCD: return "eof-in-cdata";
		case LXB_HTML_TOKENIZER_ERROR_EOINCO: return "eof-in-comment";
		case LXB_HTML_TOKENIZER_ERROR_EOINDO: return "eof-in-doctype";
		case LXB_HTML_TOKENIZER_ERROR_EOINSCHTCOLITE: return "eof-in-script-html-comment-like-text";
		case LXB_HTML_TOKENIZER_ERROR_EOINTA: return "eof-in-tag";
		case LXB_HTML_TOKENIZER_ERROR_INCLCO: return "incorrectly-closed-comment";
		case LXB_HTML_TOKENIZER_ERROR_INOPCO: return "incorrectly-opened-comment";
		case LXB_HTML_TOKENIZER_ERROR_INCHSEAFDONA: return "invalid-character-sequence-after-doctype-name";
		case LXB_HTML_TOKENIZER_ERROR_INFICHOFTANA: return "invalid-first-character-of-tag-name";
		case LXB_HTML_TOKENIZER_ERROR_MIATVA: return "missing-attribute-value";
		case LXB_HTML_TOKENIZER_ERROR_MIDONA: return "missing-doctype-name";
		case LXB_HTML_TOKENIZER_ERROR_MIDOPUID: return "missing-doctype-public-identifier";
		case LXB_HTML_TOKENIZER_ERROR_MIDOSYID: return "missing-doctype-system-identifier";
		case LXB_HTML_TOKENIZER_ERROR_MIENTANA: return "missing-end-tag-name";
		case LXB_HTML_TOKENIZER_ERROR_MIQUBEDOPUID: return "missing-quote-before-doctype-public-identifier";
		case LXB_HTML_TOKENIZER_ERROR_MIQUBEDOSYID: return "missing-quote-before-doctype-system-identifier";
		case LXB_HTML_TOKENIZER_ERROR_MISEAFCHRE: return "missing-semicolon-after-character-reference";
		case LXB_HTML_TOKENIZER_ERROR_MIWHAFDOPUKE: return "missing-whitespace-after-doctype-public-keyword";
		case LXB_HTML_TOKENIZER_ERROR_MIWHAFDOSYKE: return "missing-whitespace-after-doctype-system-keyword";
		case LXB_HTML_TOKENIZER_ERROR_MIWHBEDONA: return "missing-whitespace-before-doctype-name";
		case LXB_HTML_TOKENIZER_ERROR_MIWHBEAT: return "missing-whitespace-between-attributes";
		case LXB_HTML_TOKENIZER_ERROR_MIWHBEDOPUANSYID: return "missing-whitespace-between-doctype-public-and-system-identifiers";
		case LXB_HTML_TOKENIZER_ERROR_NECO: return "nested-comment";
		case LXB_HTML_TOKENIZER_ERROR_NOCHRE: return "noncharacter-character-reference";
		case LXB_HTML_TOKENIZER_ERROR_NOININST: return "noncharacter-in-input-stream";
		case LXB_HTML_TOKENIZER_ERROR_NOVOHTELSTTAWITRSO: return "non-void-html-element-start-tag-with-trailing-solidus";
		case LXB_HTML_TOKENIZER_ERROR_NUCHRE: return "null-character-reference";
		case LXB_HTML_TOKENIZER_ERROR_SUCHRE: return "surrogate-character-reference";
		case LXB_HTML_TOKENIZER_ERROR_SUININST: return "surrogate-in-input-stream";
		case LXB_HTML_TOKENIZER_ERROR_UNCHAFDOSYID: return "unexpected-character-after-doctype-system-identifier";
		case LXB_HTML_TOKENIZER_ERROR_UNCHINATNA: return "unexpected-character-in-attribute-name";
		case LXB_HTML_TOKENIZER_ERROR_UNCHINUNATVA: return "unexpected-character-in-unquoted-attribute-value";
		case LXB_HTML_TOKENIZER_ERROR_UNEQSIBEATNA: return "unexpected-equals-sign-before-attribute-name";
		case LXB_HTML_TOKENIZER_ERROR_UNNUCH: return "unexpected-null-character";
		case LXB_HTML_TOKENIZER_ERROR_UNQUMAINOFTANA: return "unexpected-question-mark-instead-of-tag-name";
		case LXB_HTML_TOKENIZER_ERROR_UNSOINTA: return "unexpected-solidus-in-tag";
		case LXB_HTML_TOKENIZER_ERROR_UNNACHRE: return "unknown-named-character-reference";
		default: return "unknown error";
	}
}

static const char *dom_lexbor_tree_error_code_to_string(lxb_html_tree_error_id_t id)
{
	switch (id) {
		case LXB_HTML_RULES_ERROR_UNTO: return "unexpected-token";
		case LXB_HTML_RULES_ERROR_UNCLTO: return "unexpected-closed-token";
		case LXB_HTML_RULES_ERROR_NUCH: return "null-character";
		case LXB_HTML_RULES_ERROR_UNCHTO: return "unexpected-character-token";
		case LXB_HTML_RULES_ERROR_UNTOININMO: return "unexpected-token-in-initial-mode";
		case LXB_HTML_RULES_ERROR_BADOTOININMO: return "bad-doctype-token-in-initial-mode";
		case LXB_HTML_RULES_ERROR_DOTOINBEHTMO: return "doctype-token-in-before-html-mode";
		case LXB_HTML_RULES_ERROR_UNCLTOINBEHTMO: return "unexpected-closed-token-in-before-html-mode";
		case LXB_HTML_RULES_ERROR_DOTOINBEHEMO: return "doctype-token-in-before-head-mode";
		case LXB_HTML_RULES_ERROR_UNCLTOINBEHEMO: return "unexpected-closed_token-in-before-head-mode";
		case LXB_HTML_RULES_ERROR_DOTOINHEMO: return "doctype-token-in-head-mode";
		case LXB_HTML_RULES_ERROR_NOVOHTELSTTAWITRSO: return "non-void-html-element-start-tag-with-trailing-solidus";
		case LXB_HTML_RULES_ERROR_HETOINHEMO: return "head-token-in-head-mode";
		case LXB_HTML_RULES_ERROR_UNCLTOINHEMO: return "unexpected-closed-token-in-head-mode";
		case LXB_HTML_RULES_ERROR_TECLTOWIOPINHEMO: return "template-closed-token-without-opening-in-head-mode";
		case LXB_HTML_RULES_ERROR_TEELISNOCUINHEMO: return "template-element-is-not-current-in-head-mode";
		case LXB_HTML_RULES_ERROR_DOTOINHENOMO: return "doctype-token-in-head-noscript-mode";
		case LXB_HTML_RULES_ERROR_DOTOAFHEMO: return "doctype-token-after-head-mode";
		case LXB_HTML_RULES_ERROR_HETOAFHEMO: return "head-token-after-head-mode";
		case LXB_HTML_RULES_ERROR_DOTOINBOMO: return "doctype-token-in-body-mode";
		case LXB_HTML_RULES_ERROR_BAENOPELISWR: return "bad-ending-open-elements-is-wrong";
		case LXB_HTML_RULES_ERROR_OPELISWR: return "open-elements-is-wrong";
		case LXB_HTML_RULES_ERROR_UNELINOPELST: return "unexpected-element-in-open-elements-stack";
		case LXB_HTML_RULES_ERROR_MIELINOPELST: return "missing-element-in-open-elements-stack";
		case LXB_HTML_RULES_ERROR_NOBOELINSC: return "no-body-element-in-scope";
		case LXB_HTML_RULES_ERROR_MIELINSC: return "missing-element-in-scope";
		case LXB_HTML_RULES_ERROR_UNELINSC: return "unexpected-element-in-scope";
		case LXB_HTML_RULES_ERROR_UNELINACFOST: return "unexpected-element-in-active-formatting-stack";
		case LXB_HTML_RULES_ERROR_UNENOFFI: return "unexpected-end-of-file";
		case LXB_HTML_RULES_ERROR_CHINTATE: return "characters-in-table-text";
		case LXB_HTML_RULES_ERROR_DOTOINTAMO: return "doctype-token-in-table-mode";
		case LXB_HTML_RULES_ERROR_DOTOINSEMO: return "doctype-token-in-select-mode";
		case LXB_HTML_RULES_ERROR_DOTOAFBOMO: return "doctype-token-after-body-mode";
		case LXB_HTML_RULES_ERROR_DOTOINFRMO: return "doctype-token-in-frameset-mode";
		case LXB_HTML_RULES_ERROR_DOTOAFFRMO: return "doctype-token-after-frameset-mode";
		case LXB_HTML_RULES_ERROR_DOTOFOCOMO: return "doctype-token-foreign-content-mode";
		default: return "unknown error";
	}
}

static void dom_reset_line_column_cache(dom_line_column_cache *cache)
{
	cache->last_line = 1;
	cache->last_column = 1;
	cache->last_offset = 0;
}

static void dom_find_line_and_column_using_cache(const phpdom_html_parser *p, dom_line_column_cache *cache, size_t offset)
{
	offset -= p->current_total_offset;
	if (offset > p->current_input_length) {
		offset = p->current_input_length;
	}

	size_t last_column = cache->last_column;
	size_t last_line = cache->last_line;
	size_t last_offset = cache->last_offset;

	if (p->current_input_codepoints != NULL) {
		while (last_offset < offset) {
			if (p->current_input_codepoints[last_offset] == 0x000A) {
				last_line++;
				last_column = 1;
			} else {
				last_column++;
			}
			last_offset++;
		}
	} else {
		while (last_offset < offset) {
			const lxb_char_t current = p->current_input_characters[last_offset];
			if (current == '\n') {
				last_line++;
				last_column = 1;
				last_offset++;
			} else {
				if ((current & 0xC0) != 0x80) {
					last_column++;
				}
				last_offset++;
			}
		}
	}

	cache->last_column = last_column;
	cache->last_line = last_line;
	cache->last_offset = last_offset;
}

static void phpdom_report_errors(phpdom_html_parser *p, const lxb_char_t *input_html, size_t chunk_offset)
{
	void *error;

	lexbor_array_obj_t *parse_errors = lxb_html_parser_tokenizer(p->parser)->parse_errors;
	size_t index = p->tokenizer_error_offset;
	while ((error = lexbor_array_obj_get(parse_errors, index)) != NULL) {
		lxb_html_tokenizer_error_t *token_error = error;
		size_t offset = token_error->pos - input_html + chunk_offset;
		dom_find_line_and_column_using_cache(p, &p->cache_tokenizer, offset);
		p->error_fn(p->error_app, false, dom_lexbor_tokenizer_error_code_to_string(token_error->id),
			p->cache_tokenizer.last_line, p->cache_tokenizer.last_column, 0);
		index++;
	}
	p->tokenizer_error_offset = index;

	parse_errors = lxb_html_parser_tree(p->parser)->parse_errors;
	index = p->tree_error_offset;
	while ((error = lexbor_array_obj_get(parse_errors, index)) != NULL) {
		lxb_html_tree_error_t *tree_error = error;
		size_t line = tree_error->line + 1;
		size_t column = tree_error->column + 1;
		if (!(line == 1 && p->html_no_implied && tree_error->id == LXB_HTML_RULES_ERROR_UNTOININMO)) {
			p->error_fn(p->error_app, true, dom_lexbor_tree_error_code_to_string(tree_error->id), line, column, tree_error->length);
		}
		index++;
	}
	p->tree_error_offset = index;
}

static void dom_decoding_encoding_ctx_init(dom_decoding_encoding_ctx *ctx)
{
	ctx->decode_data = ctx->encode_data = lxb_encoding_data(LXB_ENCODING_UTF_8);
	ctx->fast_path = true;
	(void) lxb_encoding_encode_init(&ctx->encode, ctx->encode_data, ctx->encoding_output, PHPDOM_CHUNK);
	(void) lxb_encoding_encode_replace_set(&ctx->encode, LXB_ENCODING_REPLACEMENT_BYTES, LXB_ENCODING_REPLACEMENT_SIZE);
	(void) lxb_encoding_decode_init(&ctx->decode, ctx->decode_data, ctx->codepoints, PHPDOM_CHUNK);
	(void) lxb_encoding_decode_replace_set(&ctx->decode, LXB_ENCODING_REPLACEMENT_BUFFER, LXB_ENCODING_REPLACEMENT_BUFFER_LEN);
}

typedef struct {
	const lxb_encoding_data_t *encoding_data;
	size_t bom_shift;
} dom_character_encoding_data;

static dom_character_encoding_data dom_determine_encoding(const char *source, size_t source_len)
{
	dom_character_encoding_data result;

	if (source_len >= 3 && source[0] == '\xEF' && source[1] == '\xBB' && source[2] == '\xBF') {
		result.encoding_data = lxb_encoding_data(LXB_ENCODING_UTF_8);
		result.bom_shift = 3;
		return result;
	} else if (source_len >= 2) {
		if (source[0] == '\xFE' && source[1] == '\xFF') {
			result.encoding_data = lxb_encoding_data(LXB_ENCODING_UTF_16BE);
			result.bom_shift = 2;
			return result;
		} else if (source[0] == '\xFF' && source[1] == '\xFE') {
			result.encoding_data = lxb_encoding_data(LXB_ENCODING_UTF_16LE);
			result.bom_shift = 2;
			return result;
		}
	}

	lxb_html_encoding_t encoding;
	lxb_status_t status = lxb_html_encoding_init(&encoding);
	if (status != LXB_STATUS_OK) {
		goto fallback_uninit;
	}
	if (source_len > 1024) {
		source_len = 1024;
	}
	status = lxb_html_encoding_determine(&encoding, (const lxb_char_t *) source, (const lxb_char_t *) source + source_len);
	if (status != LXB_STATUS_OK) {
		goto fallback;
	}
	lxb_html_encoding_entry_t *entry = lxb_html_encoding_meta_entry(&encoding, 0);
	if (entry == NULL) {
		goto fallback;
	}
	result.encoding_data = lxb_encoding_data_by_pre_name(entry->name, entry->end - entry->name);
	if (!result.encoding_data) {
		goto fallback;
	}
	result.bom_shift = 0;
	lxb_html_encoding_destroy(&encoding, false);
	return result;

fallback:
	lxb_html_encoding_destroy(&encoding, false);
fallback_uninit:
	result.encoding_data = lxb_encoding_data(DOM_FALLBACK_ENCODING_ID);
	result.bom_shift = 0;
	return result;
}

static void dom_setup_parser_encoding_manually(phpdom_html_parser *p, const lxb_char_t *buf_start, const lxb_encoding_data_t *encoding_data)
{
	static const lxb_codepoint_t replacement_codepoint = LXB_ENCODING_REPLACEMENT_CODEPOINT;
	dom_decoding_encoding_ctx *ctx = &p->dec;

	ctx->decode_data = encoding_data;
	(void) lxb_encoding_decode_init(&ctx->decode, ctx->decode_data, ctx->codepoints, PHPDOM_CHUNK);
	(void) lxb_encoding_decode_replace_set(&ctx->decode, &replacement_codepoint, LXB_ENCODING_REPLACEMENT_BUFFER_LEN);
	ctx->fast_path = ctx->decode_data == ctx->encode_data;

	if (ctx->fast_path) {
		p->current_input_codepoints = NULL;
		p->current_input_characters = (const char *) buf_start;
	} else {
		p->current_input_codepoints = ctx->codepoints;
		p->current_input_characters = NULL;
	}
}

static void dom_setup_parser_encoding_implicitly(phpdom_html_parser *p, const lxb_char_t **buf_ref, size_t *read)
{
	const char *buf_start = (const char *) *buf_ref;
	dom_character_encoding_data data = dom_determine_encoding(buf_start, *read);
	*buf_ref += data.bom_shift;
	*read -= data.bom_shift;
	dom_setup_parser_encoding_manually(p, (const lxb_char_t *) buf_start, data.encoding_data);
}

static bool dom_process_parse_chunk(phpdom_html_parser *p, size_t encoded_length, const lxb_char_t *encoding_output, size_t input_buffer_length)
{
	p->current_input_length = input_buffer_length;
	lxb_status_t status = lxb_html_document_parse_chunk(p->document, encoding_output, encoded_length);
	if (status != LXB_STATUS_OK) {
		return false;
	}
	if (p->report_errors) {
		phpdom_report_errors(p, encoding_output, p->current_total_offset);
		dom_find_line_and_column_using_cache(p, &p->cache_tokenizer, p->current_total_offset + input_buffer_length);
	}
	p->current_total_offset += input_buffer_length;
	p->cache_tokenizer.last_offset = 0;
	return true;
}

static inline bool dom_seek_utf8_non_ascii(const lxb_char_t **data, const lxb_char_t *end)
{
	while (*data + sizeof(size_t) <= end) {
		size_t bytes;
		memcpy(&bytes, *data, sizeof(bytes));
		if ((bytes & LEXBOR_SWAR_REPEAT(0x80)) != 0) {
			return false;
		}
		*data += sizeof(size_t);
	}
	while (*data < end) {
		if (**data & 0x80) {
			return false;
		}
		(*data)++;
	}
	return true;
}

static bool dom_decode_encode_fast_path(phpdom_html_parser *p, const lxb_char_t **buf_ref_ref, const lxb_char_t *buf_end)
{
	dom_decoding_encoding_ctx *ctx = &p->dec;
	const lxb_char_t *buf_ref = *buf_ref_ref;

	if (ctx->decode.status == LXB_STATUS_CONTINUE) {
		lxb_char_t buf[4];
		lxb_char_t *buf_ptr = buf;
		lxb_codepoint_t codepoint = lxb_encoding_decode_utf_8_single(&ctx->decode, &buf_ref, buf_end);
		if (lxb_encoding_encode_utf_8_single(&ctx->encode, &buf_ptr, buf + sizeof(buf), codepoint) > sizeof(buf)) {
			memcpy(buf, LXB_ENCODING_REPLACEMENT_BYTES, LXB_ENCODING_REPLACEMENT_SIZE);
			buf_ptr = buf + LXB_ENCODING_REPLACEMENT_SIZE;
		}
		ctx->decode.status = LXB_STATUS_OK;

		if (!dom_process_parse_chunk(p, buf_ptr - buf, buf, buf_ref - *buf_ref_ref)) {
			goto fail_oom;
		}
	}

	const lxb_char_t *last_output = buf_ref;
	while (buf_ref != buf_end) {
		if (ctx->decode.u.utf_8.need == 0) {
			if (dom_seek_utf8_non_ascii(&buf_ref, buf_end)) {
				break;
			}
		}
		const lxb_char_t *buf_ref_backup = buf_ref;
		lxb_codepoint_t codepoint = lxb_encoding_decode_utf_8_single(&ctx->decode, &buf_ref, buf_end);
		if (codepoint > LXB_ENCODING_MAX_CODEPOINT) {
			if (!dom_process_parse_chunk(p, buf_ref_backup - last_output, last_output, buf_ref - last_output)) {
				goto fail_oom;
			}

			if (codepoint == LXB_ENCODING_DECODE_CONTINUE) {
				*buf_ref_ref = buf_ref;
				ctx->decode.status = LXB_STATUS_CONTINUE;
				return true;
			}

			if (!dom_process_parse_chunk(p, LXB_ENCODING_REPLACEMENT_SIZE, LXB_ENCODING_REPLACEMENT_BYTES, 0)) {
				goto fail_oom;
			}

			last_output = buf_ref;
		}
	}
	if (buf_ref != last_output && !dom_process_parse_chunk(p, buf_ref - last_output, last_output, buf_ref - last_output)) {
		goto fail_oom;
	}
	*buf_ref_ref = buf_ref;
	return true;
fail_oom:
	*buf_ref_ref = buf_ref;
	return false;
}

static bool dom_decode_encode_slow_path(phpdom_html_parser *p, const lxb_char_t **buf_ref_ref, const lxb_char_t *buf_end)
{
	dom_decoding_encoding_ctx *ctx = &p->dec;
	const lxb_char_t *buf_ref = *buf_ref_ref;
	lxb_status_t decode_status, encode_status;
	do {
		decode_status = ctx->decode_data->decode(&ctx->decode, &buf_ref, buf_end);

		const lxb_codepoint_t *codepoints_ref = (const lxb_codepoint_t *) ctx->codepoints;
		size_t decoding_buffer_used = lxb_encoding_decode_buf_used(&ctx->decode);
		const lxb_codepoint_t *codepoints_end = ctx->codepoints + decoding_buffer_used;
		do {
			encode_status = ctx->encode_data->encode(&ctx->encode, &codepoints_ref, codepoints_end);
			if (!dom_process_parse_chunk(p, lxb_encoding_encode_buf_used(&ctx->encode), ctx->encoding_output, decoding_buffer_used)) {
				goto fail_oom;
			}
			lxb_encoding_encode_buf_used_set(&ctx->encode, 0);
		} while (encode_status == LXB_STATUS_SMALL_BUFFER);
		lxb_encoding_decode_buf_used_set(&ctx->decode, 0);
	} while (decode_status == LXB_STATUS_SMALL_BUFFER);
	*buf_ref_ref = buf_ref;
	return true;
fail_oom:
	*buf_ref_ref = buf_ref;
	return false;
}

static bool dom_parse_decode_encode_step(phpdom_html_parser *p, const lxb_char_t **buf_ref_ref, const lxb_char_t *buf_end)
{
	if (p->dec.fast_path) {
		return dom_decode_encode_fast_path(p, buf_ref_ref, buf_end);
	}
	return dom_decode_encode_slow_path(p, buf_ref_ref, buf_end);
}

static bool dom_parse_decode_encode_finish(phpdom_html_parser *p)
{
	dom_decoding_encoding_ctx *ctx = &p->dec;
	(void) lxb_encoding_decode_finish(&ctx->decode);

	size_t decoding_buffer_size = lxb_encoding_decode_buf_used(&ctx->decode);
	if (decoding_buffer_size > 0) {
		const lxb_codepoint_t *codepoints_ref = (const lxb_codepoint_t *) ctx->codepoints;
		const lxb_codepoint_t *codepoints_end = codepoints_ref + decoding_buffer_size;
		(void) ctx->encode_data->encode(&ctx->encode, &codepoints_ref, codepoints_end);
	}

	(void) lxb_encoding_encode_finish(&ctx->encode);
	if (lxb_encoding_encode_buf_used(&ctx->encode)
		&& !dom_process_parse_chunk(p, lxb_encoding_encode_buf_used(&ctx->encode), ctx->encoding_output, lxb_encoding_decode_buf_used(&ctx->decode))) {
		return false;
	}
	return true;
}

/* Creates a parser.  `override_encoding` may be NULL.  Returns NULL on
 * allocation failure. */
phpdom_html_parser *phpdom_html_parser_new(const lxb_encoding_data_t *override_encoding, bool report_errors, bool html_no_implied,
	phpdom_html_error_fn error_fn, void *error_app)
{
	phpdom_html_parser *p = calloc(1, sizeof(*p));
	if (p == NULL) {
		return NULL;
	}
	p->html_no_implied = html_no_implied;
	p->report_errors = report_errors && error_fn != NULL;
	p->error_fn = error_fn;
	p->error_app = error_app;
	dom_reset_line_column_cache(&p->cache_tokenizer);
	dom_decoding_encoding_ctx_init(&p->dec);
	p->override_encoding = override_encoding;
	p->determine_implicitly = override_encoding == NULL;

	p->document = lxb_html_document_create();
	if (p->document == NULL) {
		free(p);
		return NULL;
	}
	if (lxb_html_document_parse_chunk_begin(p->document) != LXB_STATUS_OK) {
		lxb_html_document_destroy(p->document);
		free(p);
		return NULL;
	}
	p->parser = p->document->dom_document.parser;
	return p;
}

void phpdom_html_parser_free(phpdom_html_parser *p)
{
	if (p == NULL) {
		return;
	}
	if (p->document != NULL) {
		lxb_html_document_destroy(p->document);
	}
	free(p);
}

/* Dom\HTMLDocument::createFromString(): the whole input at once. */
bool phpdom_html_parse_string(phpdom_html_parser *p, const char *source, size_t source_len)
{
	const lxb_char_t *buf_ref = (const lxb_char_t *) source;
	if (p->override_encoding != NULL) {
		dom_setup_parser_encoding_manually(p, buf_ref, p->override_encoding);
	} else {
		dom_setup_parser_encoding_implicitly(p, &buf_ref, &source_len);
	}

	while (source_len > 0) {
		size_t chunk_size = source_len;
		if (chunk_size > PHPDOM_CHUNK) {
			chunk_size = PHPDOM_CHUNK;
		}
		source_len -= chunk_size;

		const lxb_char_t *buf_end = buf_ref + chunk_size;
		if (!dom_parse_decode_encode_step(p, &buf_ref, buf_end)) {
			return false;
		}
		if (p->current_input_characters) {
			p->current_input_characters += chunk_size;
		}
	}

	if (!dom_parse_decode_encode_finish(p)) {
		return false;
	}
	return lxb_html_document_parse_chunk_end(p->document) == LXB_STATUS_OK;
}

/* Dom\HTMLDocument::createFromFile(): one read of at most 4096 bytes. */
bool phpdom_html_parse_file_chunk(phpdom_html_parser *p, const char *data, size_t len)
{
	if (len > PHPDOM_CHUNK) {
		len = PHPDOM_CHUNK;
	}
	if (!p->determine_implicitly && p->current_input_characters == NULL && p->current_input_codepoints == NULL) {
		/* Manual setup refers to the read buffer. */
		dom_setup_parser_encoding_manually(p, (const lxb_char_t *) p->buf, p->override_encoding);
	}
	memcpy(p->buf, data, len);
	const lxb_char_t *buf_ref = (const lxb_char_t *) p->buf;
	if (p->determine_implicitly) {
		p->determine_implicitly = false;
		dom_setup_parser_encoding_implicitly(p, &buf_ref, &len);
	}
	return dom_parse_decode_encode_step(p, &buf_ref, buf_ref + len);
}

/* Sets the encoding used for a file whose charset was sniffed from the stream. */
void phpdom_html_parser_set_encoding(phpdom_html_parser *p, const lxb_encoding_data_t *encoding)
{
	p->override_encoding = encoding;
	p->determine_implicitly = false;
}

bool phpdom_html_parse_file_finish(phpdom_html_parser *p)
{
	if (!p->determine_implicitly && p->current_input_characters == NULL && p->current_input_codepoints == NULL && p->override_encoding != NULL) {
		dom_setup_parser_encoding_manually(p, (const lxb_char_t *) p->buf, p->override_encoding);
	}
	if (!dom_parse_decode_encode_finish(p)) {
		return false;
	}
	return lxb_html_document_parse_chunk_end(p->document) == LXB_STATUS_OK;
}

lxb_dom_node_t *phpdom_html_parser_document_last_child(phpdom_html_parser *p)
{
	return lxb_dom_interface_node(p->document)->last_child;
}

const char *phpdom_html_parser_encoding_name(phpdom_html_parser *p)
{
	return p->dec.decode_data ? (const char *) p->dec.decode_data->name : "UTF-8";
}

/* 0 = no quirks, 1 = limited quirks, 2 = quirks. */
void phpdom_html_parser_observations(phpdom_html_parser *p, bool *html, bool *head, bool *body, int *quirks)
{
	lxb_html_tree_t *tree = p->parser->tree;
	*html = tree->has_explicit_html_tag;
	*head = tree->has_explicit_head_tag;
	*body = tree->has_explicit_body_tag;
	switch (lxb_dom_interface_document(tree->document)->compat_mode) {
		case LXB_DOM_DOCUMENT_CMODE_LIMITED_QUIRKS: *quirks = 1; break;
		case LXB_DOM_DOCUMENT_CMODE_QUIRKS: *quirks = 2; break;
		default: *quirks = 0; break;
	}
}

/* ------------------------------------------------------------------ */
/* Fragment parsing (inner_outer_html_mixin.c)                         */
/* ------------------------------------------------------------------ */

static lxb_dom_node_t *dom_html_fragment_lexbor_parse(lxb_html_document_t *document, lxb_dom_element_t *element, const char *input, size_t input_len)
{
	lxb_status_t status = lxb_html_document_parse_fragment_chunk_begin(document, element);
	if (status != LXB_STATUS_OK) {
		return NULL;
	}

	const lxb_encoding_data_t *encoding_data = lxb_encoding_data(LXB_ENCODING_UTF_8);
	lxb_encoding_decode_t decode;
	lxb_encoding_decode_init_single(&decode, encoding_data);

	const lxb_char_t *buf_ref = (const lxb_char_t *) input;
	const lxb_char_t *buf_end = buf_ref + input_len;
	const lxb_char_t *last_output = buf_ref;
	while (buf_ref < buf_end) {
		if (decode.u.utf_8.need == 0 && *buf_ref < 0x80) {
			buf_ref++;
			continue;
		}

		const lxb_char_t *buf_ref_backup = buf_ref;
		lxb_codepoint_t codepoint = lxb_encoding_decode_utf_8_single(&decode, &buf_ref, buf_end);
		if (codepoint > LXB_ENCODING_MAX_CODEPOINT) {
			status = lxb_html_document_parse_fragment_chunk(document, last_output, buf_ref_backup - last_output);
			if (status != LXB_STATUS_OK) {
				return NULL;
			}
			status = lxb_html_document_parse_fragment_chunk(document, LXB_ENCODING_REPLACEMENT_BYTES, LXB_ENCODING_REPLACEMENT_SIZE);
			if (status != LXB_STATUS_OK) {
				return NULL;
			}
			last_output = buf_ref;
		}
	}

	if (buf_ref != last_output) {
		status = lxb_html_document_parse_fragment_chunk(document, last_output, buf_ref - last_output);
		if (status != LXB_STATUS_OK) {
			return NULL;
		}
	}

	return lxb_html_document_parse_fragment_chunk_end(document);
}

/* The HTML fragment parsing algorithm.  Returns the lexbor document (to be
 * destroyed with phpdom_html_document_destroy) and the root node in
 * `*root_out` (NULL on failure). */
lxb_html_document_t *phpdom_html_fragment_parse(int quirks_mode, const char *context_name, size_t context_name_len,
	const char *ns_uri, size_t ns_uri_len, const char *input, size_t input_len, lxb_dom_node_t **root_out)
{
	*root_out = NULL;
	lxb_html_document_t *document = lxb_html_document_create();
	if (document == NULL) {
		return NULL;
	}
	switch (quirks_mode) {
		case 1: document->dom_document.compat_mode = LXB_DOM_DOCUMENT_CMODE_LIMITED_QUIRKS; break;
		case 2: document->dom_document.compat_mode = LXB_DOM_DOCUMENT_CMODE_QUIRKS; break;
		default: document->dom_document.compat_mode = LXB_DOM_DOCUMENT_CMODE_NO_QUIRKS; break;
	}
	lxb_dom_element_t *element = lxb_dom_element_interface_create(&document->dom_document);
	if (element == NULL) {
		return document;
	}

	const lxb_tag_data_t *tag_data = lxb_tag_data_by_name(document->dom_document.tags, (const lxb_char_t *) context_name, context_name_len);
	element->node.local_name = tag_data == NULL ? LXB_TAG__UNDEF : tag_data->tag_id;

	const lxb_ns_data_t *ns_data = lxb_ns_data_by_link(document->dom_document.ns, (const lxb_char_t *) ns_uri, ns_uri_len);
	element->node.ns = ns_data == NULL ? LXB_NS__UNDEF : ns_data->ns_id;

	*root_out = dom_html_fragment_lexbor_parse(document, element, input, input_len);
	return document;
}

void phpdom_html_document_destroy(lxb_html_document_t *document)
{
	if (document != NULL) {
		lxb_html_document_destroy(document);
	}
}

/* ------------------------------------------------------------------ */
/* Read-only accessors for the lexbor -> libxml2 bridge                */
/* ------------------------------------------------------------------ */

void phpdom_lxb_ns_ids(uintptr_t out[6])
{
	out[0] = LXB_NS__UNDEF;
	out[1] = LXB_NS_HTML;
	out[2] = LXB_NS_SVG;
	out[3] = LXB_NS_MATH;
	out[4] = LXB_NS_XLINK;
	out[5] = LXB_NS_XML;
}

uintptr_t phpdom_lxb_ns_xmlns(void)
{
	return LXB_NS_XMLNS;
}

int phpdom_lxb_node_type(const lxb_dom_node_t *node)
{
	return (int) node->type;
}

lxb_dom_node_t *phpdom_lxb_node_prev(const lxb_dom_node_t *node)
{
	return node->prev;
}

lxb_dom_node_t *phpdom_lxb_node_last_child(const lxb_dom_node_t *node)
{
	return node->last_child;
}

size_t phpdom_lxb_node_line(const lxb_dom_node_t *node)
{
	return node->line;
}

uintptr_t phpdom_lxb_node_ns(const lxb_dom_node_t *node)
{
	return node->ns;
}

const lxb_char_t *phpdom_lxb_element_qname(lxb_dom_node_t *node, size_t *len)
{
	return lxb_dom_element_qualified_name(lxb_dom_interface_element(node), len);
}

bool phpdom_lxb_is_template(lxb_dom_node_t *node)
{
	return lxb_html_tree_node_is(node, LXB_TAG_TEMPLATE);
}

/* The last child of a template's contents, or NULL with *has_content false
 * when it has no content fragment. */
lxb_dom_node_t *phpdom_lxb_template_content_last_child(lxb_dom_node_t *node, bool *has_content)
{
	lxb_html_template_element_t *template = lxb_html_interface_template(node);
	if (template->content == NULL) {
		*has_content = false;
		return NULL;
	}
	*has_content = true;
	return template->content->node.last_child;
}

lxb_dom_attr_t *phpdom_lxb_first_attr(lxb_dom_node_t *node)
{
	return lxb_dom_interface_element(node)->first_attr;
}

lxb_dom_attr_t *phpdom_lxb_attr_next(const lxb_dom_attr_t *attr)
{
	return attr->next;
}

const lxb_char_t *phpdom_lxb_attr_qname(lxb_dom_attr_t *attr, size_t *len)
{
	return lxb_dom_attr_qualified_name(attr, len);
}

bool phpdom_lxb_attr_has_prefix(const lxb_dom_attr_t *attr)
{
	return attr->node.prefix != 0;
}

uintptr_t phpdom_lxb_attr_ns(const lxb_dom_attr_t *attr)
{
	return attr->node.ns;
}

const lxb_char_t *phpdom_lxb_attr_value(lxb_dom_attr_t *attr, size_t *len)
{
	return lxb_dom_attr_value(attr, len);
}

const lxb_char_t *phpdom_lxb_chardata(lxb_dom_node_t *node, size_t *len)
{
	lxb_dom_character_data_t *data = lxb_dom_interface_character_data(node);
	*len = data->data.length;
	return data->data.data;
}

const lxb_char_t *phpdom_lxb_doctype_name(lxb_dom_node_t *node, size_t *len)
{
	return lxb_dom_document_type_name(lxb_dom_interface_document_type(node), len);
}

const lxb_char_t *phpdom_lxb_doctype_public_id(lxb_dom_node_t *node, size_t *len)
{
	return lxb_dom_document_type_public_id(lxb_dom_interface_document_type(node), len);
}

const lxb_char_t *phpdom_lxb_doctype_system_id(lxb_dom_node_t *node, size_t *len)
{
	return lxb_dom_document_type_system_id(lxb_dom_interface_document_type(node), len);
}

/* ------------------------------------------------------------------ */
/* The bridge's work list (php-src html5_parser.c `work_list`)          */
/* ------------------------------------------------------------------ */

/*
 * The lexbor -> libxml2 bridge (Rust, html5_parser.rs) keeps the nodes it
 * still has to convert in a lexbor_array_obj_t, as php-src's
 * lexbor_libxml2_bridge_convert does.  Using lexbor's own array gives the
 * list php-src's memory footprint (lexbor sizes it as
 * sizeof(uint8_t *) * items * item size and grows it 128 items at a time)
 * and its fallibility, so a huge flat document reaches memory_limit while
 * it is converted, as it does in PHP (GH-25118).
 */

#define PHPDOM_WORK_LIST_INIT_SIZE 128

/* `work_list_item`; mirrored by WorkItemRaw in lexbor.rs. */
typedef struct phpdom_work_item {
	const lxb_dom_node_t *node;
	uintptr_t current_active_namespace;
	void *lxml_parent;
	void *lxml_ns;
	bool in_template;
} phpdom_work_item;

/* `lexbor_array_obj_init(&work_list, WORK_LIST_INIT_SIZE, sizeof(work_list_item))`; NULL on allocation failure. */
lexbor_array_obj_t *phpdom_work_list_create(void)
{
	lexbor_array_obj_t *list = lexbor_array_obj_create();
	if (list == NULL) {
		return NULL;
	}
	if (lexbor_array_obj_init(list, PHPDOM_WORK_LIST_INIT_SIZE, sizeof(phpdom_work_item)) != LXB_STATUS_OK) {
		lexbor_array_obj_destroy(list, true);
		return NULL;
	}
	return list;
}

/* `lexbor_libxml2_bridge_work_list_item_push`: false when the list could not grow. */
bool phpdom_work_list_push(lexbor_array_obj_t *list, const lxb_dom_node_t *node, uintptr_t current_active_namespace,
	void *lxml_parent, void *lxml_ns, bool in_template)
{
	phpdom_work_item *item = (phpdom_work_item *) lexbor_array_obj_push_wo_cls(list);
	if (item == NULL) {
		return false;
	}
	item->node = node;
	item->current_active_namespace = current_active_namespace;
	item->lxml_parent = lxml_parent;
	item->lxml_ns = lxml_ns;
	item->in_template = in_template;
	return true;
}

/* `lexbor_array_obj_pop`: copies the most recently pushed item into *out; false when the list is empty. */
bool phpdom_work_list_pop(lexbor_array_obj_t *list, phpdom_work_item *out)
{
	const phpdom_work_item *item = (const phpdom_work_item *) lexbor_array_obj_pop(list);
	if (item == NULL) {
		return false;
	}
	*out = *item;
	return true;
}

void phpdom_work_list_destroy(lexbor_array_obj_t *list)
{
	lexbor_array_obj_destroy(list, true);
}
