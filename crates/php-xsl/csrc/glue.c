/*
 * C glue for php-xsl.
 *
 * 1. libxslt and libxml2 report errors through printf-style *variadic*
 *    callbacks (xmlGenericErrorFunc). Defining a C-variadic function is not
 *    possible in stable Rust, so the handlers PHP installs
 *    (xsl_libxslt_error_handler in ext/xsl/php_xsl.c and
 *    php_libxml_error_handler in ext/libxml/libxml.c) live here: they format
 *    the message and hand the bytes to a Rust sink.
 * 2. Accessors for fields of libxslt/libxml2 structures, so that the Rust
 *    side never depends on their layout.
 * 3. The libxml2 parser-global juggling PHP does around stylesheet
 *    compilation (PHP_LIBXML_SANITIZE_GLOBALS + the XSL overrides).
 */

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libxml/globals.h>
#include <libxml/parser.h>
#include <libxml/tree.h>
#include <libxml/xpath.h>
#include <libxml/xpathInternals.h>
#include <libxslt/xsltconfig.h>
#include <libxslt/xsltInternals.h>
#include <libxslt/transform.h>
#include <libxslt/xsltutils.h>
#include <libexslt/exsltconfig.h>

/* Implemented in Rust (src/raw.rs). `replace` is non-zero when the message
 * came through the "%s" format (libxslt's xsltTransformError path), where
 * PHP rewrites libxslt option names into PHP property names. */
extern void php_xsl_rs_error_sink(int replace, const char *msg, size_t len);

static void php_xsl_sink_va(int replace, const char *fmt, va_list ap)
{
	char stack_buf[1024];
	va_list copy;
	int n;

	va_copy(copy, ap);
	n = vsnprintf(stack_buf, sizeof(stack_buf), fmt, copy);
	va_end(copy);
	if (n < 0) {
		return;
	}
	if ((size_t) n < sizeof(stack_buf)) {
		php_xsl_rs_error_sink(replace, stack_buf, (size_t) n);
		return;
	}
	char *buf = malloc((size_t) n + 1);
	if (buf == NULL) {
		return;
	}
	vsnprintf(buf, (size_t) n + 1, fmt, ap);
	php_xsl_rs_error_sink(replace, buf, (size_t) n);
	free(buf);
}

/* xsl_libxslt_error_handler */
static void php_xsl_libxslt_error_handler(void *ctx, const char *msg, ...)
{
	va_list args;
	(void) ctx;
	va_start(args, msg);
	if (msg != NULL && strcmp(msg, "%s") == 0) {
		const char *arg = va_arg(args, const char *);
		if (arg == NULL) {
			arg = "(null)";
		}
		php_xsl_rs_error_sink(1, arg, strlen(arg));
	} else if (msg != NULL) {
		php_xsl_sink_va(0, msg, args);
	}
	va_end(args);
}

/* php_libxml_error_handler (generic channel) */
static void php_xsl_libxml_error_handler(void *ctx, const char *msg, ...)
{
	va_list args;
	(void) ctx;
	if (msg == NULL) {
		return;
	}
	va_start(args, msg);
	php_xsl_sink_va(0, msg, args);
	va_end(args);
}

void php_xsl_glue_install_xslt_error_handler(void)
{
	xsltSetGenericErrorFunc(NULL, php_xsl_libxslt_error_handler);
}

/* Installs php_libxml_error_handler as libxml2's (per-thread) generic error
 * handler, saving the previous one into saved[0..1]. */
void php_xsl_glue_push_libxml_generic(void **saved)
{
	saved[0] = (void *) xmlGenericError;
	saved[1] = xmlGenericErrorContext;
	xmlSetGenericErrorFunc(NULL, php_xsl_libxml_error_handler);
}

void php_xsl_glue_pop_libxml_generic(void **saved)
{
	xmlSetGenericErrorFunc(saved[1], (xmlGenericErrorFunc) saved[0]);
}

/* PHP_LIBXML_SANITIZE_GLOBALS(parse) followed by the XSL overrides
 * (xmlSubstituteEntitiesDefault(1), xmlLoadExtDtdDefaultValue =
 * XML_DETECT_IDS | XML_COMPLETE_ATTRS). saved must hold 6 ints. */
void php_xsl_glue_sanitize_globals(int *saved)
{
#if defined(__GNUC__)
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
#endif
	saved[0] = xmlLoadExtDtdDefaultValue;
	xmlLoadExtDtdDefaultValue = 0;
	saved[1] = xmlDoValidityCheckingDefaultValue;
	xmlDoValidityCheckingDefaultValue = 0;
	saved[2] = xmlPedanticParserDefault(0);
	saved[3] = xmlSubstituteEntitiesDefault(0);
	saved[4] = xmlLineNumbersDefault(0);
	saved[5] = xmlKeepBlanksDefault(1);
	xmlSubstituteEntitiesDefault(1);
	xmlLoadExtDtdDefaultValue = XML_DETECT_IDS | XML_COMPLETE_ATTRS;
#if defined(__GNUC__)
#pragma GCC diagnostic pop
#endif
}

/* PHP_LIBXML_RESTORE_GLOBALS(parse) */
void php_xsl_glue_restore_globals(const int *saved)
{
#if defined(__GNUC__)
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
#endif
	xmlLoadExtDtdDefaultValue = saved[0];
	xmlDoValidityCheckingDefaultValue = saved[1];
	(void) xmlPedanticParserDefault(saved[2]);
	(void) xmlSubstituteEntitiesDefault(saved[3]);
	(void) xmlLineNumbersDefault(saved[4]);
	(void) xmlKeepBlanksDefault(saved[5]);
#if defined(__GNUC__)
#pragma GCC diagnostic pop
#endif
}

/* ---- compile-time configuration ---------------------------------------- */

int php_xsl_glue_libxslt_version(void) { return LIBXSLT_VERSION; }
const char *php_xsl_glue_libxslt_dotted_version(void) { return LIBXSLT_DOTTED_VERSION; }
int php_xsl_glue_libexslt_version(void) { return LIBEXSLT_VERSION; }
const char *php_xsl_glue_libexslt_dotted_version(void) { return LIBEXSLT_DOTTED_VERSION; }

/* ---- xsltStylesheet / xsltTransformContext ------------------------------ */

xmlDocPtr php_xsl_glue_sheet_doc(xsltStylesheetPtr sheet)
{
	return sheet ? sheet->doc : NULL;
}

void php_xsl_glue_ctxt_configure(xsltTransformContextPtr ctxt, void *priv, int xinclude,
	int max_template_depth, int max_template_vars)
{
	ctxt->_private = priv;
	ctxt->xinclude = xinclude;
	ctxt->maxTemplateDepth = max_template_depth;
	ctxt->maxTemplateVars = max_template_vars;
}

void *php_xsl_glue_ctxt_private(xsltTransformContextPtr ctxt)
{
	return ctxt ? ctxt->_private : NULL;
}

void php_xsl_glue_ctxt_clear_private(xsltTransformContextPtr ctxt)
{
	if (ctxt) {
		ctxt->_private = NULL;
	}
}

xmlDocPtr php_xsl_glue_ctxt_main_doc(xsltTransformContextPtr ctxt)
{
	if (ctxt == NULL || ctxt->document == NULL) {
		return NULL;
	}
	return ctxt->document->doc;
}

/* ---- XPath parser context / objects ------------------------------------ */

const xmlChar *php_xsl_glue_xpath_function(xmlXPathParserContextPtr ctxt)
{
	return (ctxt && ctxt->context) ? ctxt->context->function : NULL;
}

const xmlChar *php_xsl_glue_xpath_function_uri(xmlXPathParserContextPtr ctxt)
{
	return (ctxt && ctxt->context) ? ctxt->context->functionURI : NULL;
}

xmlDocPtr php_xsl_glue_xpath_doc(xmlXPathParserContextPtr ctxt)
{
	return (ctxt && ctxt->context) ? ctxt->context->doc : NULL;
}

int php_xsl_glue_xpobj_type(xmlXPathObjectPtr obj) { return (int) obj->type; }
const xmlChar *php_xsl_glue_xpobj_stringval(xmlXPathObjectPtr obj) { return obj->stringval; }
int php_xsl_glue_xpobj_boolval(xmlXPathObjectPtr obj) { return obj->boolval; }
double php_xsl_glue_xpobj_floatval(xmlXPathObjectPtr obj) { return obj->floatval; }

int php_xsl_glue_xpobj_node_count(xmlXPathObjectPtr obj)
{
	return obj->nodesetval ? obj->nodesetval->nodeNr : 0;
}

xmlNodePtr php_xsl_glue_xpobj_node(xmlXPathObjectPtr obj, int i)
{
	if (obj->nodesetval == NULL || i < 0 || i >= obj->nodesetval->nodeNr) {
		return NULL;
	}
	return obj->nodesetval->nodeTab[i];
}

/* ---- nodes ---------------------------------------------------------- */

int php_xsl_glue_node_type(xmlNodePtr node) { return (int) node->type; }
xmlDocPtr php_xsl_glue_node_doc(xmlNodePtr node) { return node->doc; }
const xmlChar *php_xsl_glue_node_name(xmlNodePtr node) { return node->name; }
xmlNodePtr php_xsl_glue_node_children(xmlNodePtr node) { return node->children; }
xmlNodePtr php_xsl_glue_node_next(xmlNodePtr node) { return node->next; }
xmlNodePtr php_xsl_glue_node_parent(xmlNodePtr node) { return node->parent; }

const xmlChar *php_xsl_glue_node_ns_href(xmlNodePtr node)
{
	return node->ns ? node->ns->href : NULL;
}

/* An XPath namespace node (xmlXPathNodeSetDupNs) stores its parent element
 * in `next`. */
xmlNodePtr php_xsl_glue_nsnode_parent(xmlNsPtr ns) { return (xmlNodePtr) ns->next; }
const xmlChar *php_xsl_glue_nsnode_prefix(xmlNsPtr ns) { return ns->prefix; }
const xmlChar *php_xsl_glue_nsnode_href(xmlNsPtr ns) { return ns->href; }

/* xsl_add_ns_def: turns modern-DOM xmlns attributes back into namespace
 * declarations (`nsDef`) so libxslt sees them. */
static void php_xsl_add_ns_def(xmlNodePtr node)
{
	static const xmlChar xmlns_uri[] = "http://www.w3.org/2000/xmlns/";
	if (node->type != XML_ELEMENT_NODE) {
		return;
	}
	for (xmlAttrPtr attr = node->properties; attr; attr = attr->next) {
		if (attr->ns == NULL || !xmlStrEqual(attr->ns->href, xmlns_uri)) {
			continue;
		}
		xmlNsPtr ns = xmlMalloc(sizeof(*ns));
		if (!ns) {
			return;
		}
		xmlChar *value = xmlNodeListGetString(attr->doc, attr->children, 1);
		memset(ns, 0, sizeof(*ns));
		ns->type = XML_LOCAL_NAMESPACE;
		ns->href = value ? value : xmlStrdup((const xmlChar *) "");
		ns->prefix = attr->ns->prefix ? xmlStrdup(attr->name) : NULL;
		ns->next = node->nsDef;
		node->nsDef = ns;
	}
}

void php_xsl_glue_add_ns_defs(xmlDocPtr doc)
{
	xmlNodePtr base = (xmlNodePtr) doc;
	xmlNodePtr node = base->children;
	while (node != NULL) {
		php_xsl_add_ns_def(node);
		/* php_dom_next_in_tree_order */
		if (node->type == XML_ELEMENT_NODE && node->children) {
			node = node->children;
			continue;
		}
		while (node != NULL && node != base) {
			if (node->next) {
				node = node->next;
				break;
			}
			node = node->parent;
		}
		if (node == base) {
			break;
		}
	}
}

/* The modern-DOM variant for a document copied with xmlCopyDoc(): modern
 * documents keep their namespaces outside of nsDef, so the copy loses the
 * namespace of every xmlns attribute (xmlCopyProp only finds namespaces
 * through nsDef). Walks the original and the copy in parallel (xmlCopyDoc
 * preserves the structure) and adds the declarations of the original's
 * xmlns attributes to the copy, dropping the copied attributes that lost
 * their namespace (they would otherwise appear as plain attributes). */
static void php_xsl_add_ns_def_pair(xmlNodePtr orig, xmlNodePtr copy)
{
	static const xmlChar xmlns_uri[] = "http://www.w3.org/2000/xmlns/";
	if (orig->type != XML_ELEMENT_NODE || copy->type != XML_ELEMENT_NODE) {
		return;
	}
	xmlAttrPtr oa = orig->properties;
	xmlAttrPtr ca = copy->properties;
	while (oa != NULL && ca != NULL) {
		xmlAttrPtr cnext = ca->next;
		if (oa->ns != NULL && xmlStrEqual(oa->ns->href, xmlns_uri)) {
			xmlNsPtr ns = xmlMalloc(sizeof(*ns));
			if (!ns) {
				return;
			}
			xmlChar *value = xmlNodeListGetString(orig->doc, oa->children, 1);
			memset(ns, 0, sizeof(*ns));
			ns->type = XML_LOCAL_NAMESPACE;
			ns->href = value ? value : xmlStrdup((const xmlChar *) "");
			ns->prefix = oa->ns->prefix ? xmlStrdup(oa->name) : NULL;
			ns->next = copy->nsDef;
			copy->nsDef = ns;
			if (ca->ns == NULL) {
				xmlUnlinkNode((xmlNodePtr) ca);
				xmlFreeProp(ca);
			}
		}
		oa = oa->next;
		ca = cnext;
	}
}

static xmlNodePtr php_xsl_next_in_tree(xmlNodePtr node, xmlNodePtr base)
{
	if (node->type == XML_ELEMENT_NODE && node->children) {
		return node->children;
	}
	while (node != NULL && node != base) {
		if (node->next) {
			return node->next;
		}
		node = node->parent;
	}
	return NULL;
}

void php_xsl_glue_add_ns_defs_from(xmlDocPtr orig, xmlDocPtr copy)
{
	xmlNodePtr obase = (xmlNodePtr) orig;
	xmlNodePtr cbase = (xmlNodePtr) copy;
	xmlNodePtr o = obase->children;
	xmlNodePtr c = cbase->children;
	while (o != NULL && c != NULL) {
		if (o->type != c->type) {
			return; /* not a structural copy: give up */
		}
		php_xsl_add_ns_def_pair(o, c);
		o = php_xsl_next_in_tree(o, obase);
		c = php_xsl_next_in_tree(c, cbase);
	}
}

/* Stops the transformation (after a Rust panic in a callback). */
void php_xsl_glue_ctxt_stop(xsltTransformContextPtr ctxt)
{
	if (ctxt) {
		ctxt->state = XSLT_STATE_STOPPED;
	}
}
