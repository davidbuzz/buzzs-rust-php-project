/*
 * Derived from php-src sapi/apache2handler/mod_php.c
 *   Copyright © The PHP Group and Contributors.
 *   Authors: Sascha Schumann <sascha@schumann.cx>
 *            Parts based on Apache 1.3 SAPI module by
 *            Rasmus Lerdorf and Zeev Suraski
 * Derived from php-src sapi/apache2handler/apache_config.c
 *   Copyright © The PHP Group and Contributors.
 *   Author: Sascha Schumann <sascha@schumann.cx>
 * Derived from php-src sapi/apache2handler/sapi_apache2.c
 *   Copyright © The PHP Group and Contributors.
 *   Authors: Sascha Schumann <sascha@schumann.cx>
 *            Parts based on Apache 1.3 SAPI module by
 *            Rasmus Lerdorf and Zeev Suraski
 * Port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
 * SPDX-License-Identifier: BSD-3-Clause
 */

/*
 * mod_phprust: the httpd side of the phprust apache2handler SAPI.
 *
 * This shim is deliberately small (ref: sapi/apache2handler/mod_php.c,
 * apache_config.c and the hook registration of sapi_apache2.c).  It
 *   - declares the module (`php_module`, so `LoadModule php_module ...`
 *     and `<IfModule mod_php.c>` work as with mod_php),
 *   - implements the php_value / php_flag / php_admin_value /
 *     php_admin_flag / PHPINIDir / PhpRustAllowThreads directives with
 *     apache_config.c's merge semantics,
 *   - registers the pre_config / post_config / child_init / handler hooks,
 *   - and exposes request_rec accessors to Rust through a function table
 *     (phprs_api.h).  All request logic lives in Rust (src/).
 */

#include <string.h>
#include <limits.h>

#include "httpd.h"
#include "http_config.h"
#include "http_core.h"
#include "http_log.h"
#include "http_main.h"
#include "http_protocol.h"
#include "http_request.h"
#include "util_script.h"
#include "ap_mpm.h"
#include "apr_strings.h"
#include "apr_tables.h"

#include "phprs_api.h"

/* Per-module logging: messages are tagged [php:level] like mod_php's. */
APLOG_USE_MODULE(php);

/*
 * `php_module` itself is storage exported by the Rust cdylib (rustc's
 * version script only exports Rust symbols).  The constructor below copies
 * this fully initialised template into it when the DSO is loaded, before
 * mod_so looks the symbol up.
 */
#define PHPRS_MODULE_STORAGE 512
_Static_assert(sizeof(module) <= PHPRS_MODULE_STORAGE, "module storage too small");

/* ---- Rust entry points (src/ffi.rs) ---- */
extern void phprust_set_api(const phprs_api *api);
extern int phprust_post_config(const char *ini_dir);
extern const char *phprust_version_component(void);
extern void phprust_child_init(void);
extern int phprust_handler(request_rec *r);

/* ---- per-directory configuration (apache_config.c) ---- */

/* PHP_INI_PERDIR / PHP_INI_SYSTEM */
#define PHPRS_INI_PERDIR 2
#define PHPRS_INI_SYSTEM 4

typedef struct {
    const char *name;
    const char *value;
    int status;
    int htaccess;
} phprs_dir_entry;

typedef struct {
    apr_array_header_t *entries; /* of phprs_dir_entry, insertion ordered */
} phprs_dir_conf;

/* PHPINIDir (first one wins) and PhpRustAllowThreads. */
static const char *phprs_ini_dir = NULL;
static int phprs_allow_threads = 0;

static void *create_php_config(apr_pool_t *p, char *dummy)
{
    phprs_dir_conf *c = apr_pcalloc(p, sizeof(*c));
    (void)dummy;
    c->entries = apr_array_make(p, 4, sizeof(phprs_dir_entry));
    return c;
}

static phprs_dir_entry *find_entry(phprs_dir_conf *c, const char *name)
{
    int i;
    phprs_dir_entry *e = (phprs_dir_entry *)c->entries->elts;
    for (i = 0; i < c->entries->nelts; i++) {
        if (strcmp(e[i].name, name) == 0) {
            return &e[i];
        }
    }
    return NULL;
}

/* zend_hash_str_update: replace in place or append. */
static void put_entry(phprs_dir_conf *c, const phprs_dir_entry *n)
{
    phprs_dir_entry *e = find_entry(c, n->name);
    if (e) {
        *e = *n;
    } else {
        *(phprs_dir_entry *)apr_array_push(c->entries) = *n;
    }
}

/* merge_php_config(): copy the base, then merge the new entries unless an
 * existing entry has a higher status (php_admin_* beats php_*). */
static void *merge_php_config(apr_pool_t *p, void *base_conf, void *new_conf)
{
    phprs_dir_conf *d = base_conf, *e = new_conf;
    phprs_dir_conf *n = create_php_config(p, "merge");
    int i;
    phprs_dir_entry *be = (phprs_dir_entry *)d->entries->elts;
    phprs_dir_entry *ne = (phprs_dir_entry *)e->entries->elts;

    for (i = 0; i < d->entries->nelts; i++) {
        *(phprs_dir_entry *)apr_array_push(n->entries) = be[i];
    }
    for (i = 0; i < e->entries->nelts; i++) {
        phprs_dir_entry *orig = find_entry(n, ne[i].name);
        if (!orig || ne[i].status >= orig->status) {
            put_entry(n, &ne[i]);
        }
    }
    return n;
}

static const char *real_value_hnd(cmd_parms *cmd, void *dummy, const char *name,
                                  const char *value, int status)
{
    phprs_dir_conf *d = dummy;
    phprs_dir_entry e;

    if (!strcasecmp(value, "none")) {
        value = "";
    }
    e.name = apr_pstrdup(cmd->pool, name);
    e.value = apr_pstrdup(cmd->pool, value);
    e.status = status;
    e.htaccess = ((cmd->override & (RSRC_CONF | ACCESS_CONF)) == 0);
    put_entry(d, &e);
    return NULL;
}

static const char *real_flag_hnd(cmd_parms *cmd, void *dummy, const char *arg1,
                                 const char *arg2, int status)
{
    const char *v = (!strcasecmp(arg2, "On") || (arg2[0] == '1' && arg2[1] == '\0')) ? "1" : "0";
    return real_value_hnd(cmd, dummy, arg1, v, status);
}

static const char *php_value_hnd(cmd_parms *cmd, void *d, const char *n, const char *v)
{
    return real_value_hnd(cmd, d, n, v, PHPRS_INI_PERDIR);
}

static const char *php_admin_value_hnd(cmd_parms *cmd, void *d, const char *n, const char *v)
{
    return real_value_hnd(cmd, d, n, v, PHPRS_INI_SYSTEM);
}

static const char *php_flag_hnd(cmd_parms *cmd, void *d, const char *n, const char *v)
{
    return real_flag_hnd(cmd, d, n, v, PHPRS_INI_PERDIR);
}

static const char *php_admin_flag_hnd(cmd_parms *cmd, void *d, const char *n, const char *v)
{
    return real_flag_hnd(cmd, d, n, v, PHPRS_INI_SYSTEM);
}

static const char *php_ini_dir_hnd(cmd_parms *cmd, void *mconfig, const char *arg)
{
    (void)mconfig;
    if (phprs_ini_dir) {
        return "Only first PHPINIDir directive honored per configuration tree - subsequent ones ignored";
    }
    phprs_ini_dir = ap_server_root_relative(cmd->pool, arg);
    return NULL;
}

static const char *php_allow_threads_hnd(cmd_parms *cmd, void *mconfig, int on)
{
    (void)cmd;
    (void)mconfig;
    phprs_allow_threads = on;
    return NULL;
}

static const command_rec php_dir_cmds[] = {
    AP_INIT_TAKE2("php_value", php_value_hnd, NULL, OR_OPTIONS, "PHP Value Modifier"),
    AP_INIT_TAKE2("php_flag", php_flag_hnd, NULL, OR_OPTIONS, "PHP Flag Modifier"),
    AP_INIT_TAKE2("php_admin_value", php_admin_value_hnd, NULL, ACCESS_CONF | RSRC_CONF, "PHP Value Modifier (Admin)"),
    AP_INIT_TAKE2("php_admin_flag", php_admin_flag_hnd, NULL, ACCESS_CONF | RSRC_CONF, "PHP Flag Modifier (Admin)"),
    AP_INIT_TAKE1("PHPINIDir", php_ini_dir_hnd, NULL, RSRC_CONF, "Directory containing the php.ini file"),
    AP_INIT_FLAG("PhpRustAllowThreads", php_allow_threads_hnd, NULL, RSRC_CONF,
                 "Allow loading under a threaded MPM (requests are serialised per process)"),
    {NULL}
};

/* ---- request_rec accessors for Rust ---- */

static const char *api_r_str(void *rp, int field)
{
    request_rec *r = rp;
    switch (field) {
    case PHPRS_R_HANDLER: return r->handler;
    case PHPRS_R_FILENAME: return r->filename;
    case PHPRS_R_URI: return r->uri;
    case PHPRS_R_ARGS: return r->args;
    case PHPRS_R_METHOD: return r->method;
    case PHPRS_R_PROTOCOL: return r->protocol;
    case PHPRS_R_USER: return r->user;
    case PHPRS_R_PATH_INFO: return r->path_info;
    case PHPRS_R_THE_REQUEST: return r->the_request;
    case PHPRS_R_STATUS_LINE: return r->status_line;
    case PHPRS_R_CONTENT_TYPE: return r->content_type;
    case PHPRS_R_UNPARSED_URI: return r->unparsed_uri;
    case PHPRS_R_RANGE: return r->range;
    default: return NULL;
    }
}

static long long api_r_int(void *rp, int field)
{
    request_rec *r = rp;
    switch (field) {
    case PHPRS_R_PROTO_NUM: return r->proto_num;
    case PHPRS_R_REQUEST_TIME: return (long long)r->request_time;
    case PHPRS_R_FILETYPE:
        switch (r->finfo.filetype) {
        case APR_NOFILE: return 0;
        case APR_REG: return 1;
        case APR_DIR: return 2;
        default: return 3;
        }
    case PHPRS_R_UEXEC: return (r->finfo.protection & APR_UEXECUTE) != 0;
    case PHPRS_R_PATH_INFO_REJECTED:
        return r->used_path_info == AP_REQ_REJECT_PATH_INFO && r->path_info && r->path_info[0];
    case PHPRS_R_IS_MAIN: return r->main == NULL;
    case PHPRS_R_STATUS: return r->status;
    case PHPRS_R_MTIME: return (long long)r->mtime;
    case PHPRS_R_CLENGTH: return (long long)r->clength;
    case PHPRS_R_CHUNKED: return r->chunked;
    case PHPRS_R_NO_CACHE: return r->no_cache;
    case PHPRS_R_NO_LOCAL_COPY: return r->no_local_copy;
    case PHPRS_R_ALLOWED: return (long long)r->allowed;
    case PHPRS_R_SENT_BODYCT: return r->sent_bodyct;
    case PHPRS_R_BYTES_SENT: return (long long)r->bytes_sent;
    case PHPRS_R_ABORTED: return r->connection->aborted;
    case PHPRS_R_ENV_DIFFERS: return r->main != NULL && r->subprocess_env != r->main->subprocess_env;
    case PHPRS_R_HEADER_ONLY: return r->header_only;
    default: return 0;
    }
}

static void api_set_user(void *rp, const char *user)
{
    request_rec *r = rp;
    r->user = user ? apr_pstrdup(r->pool, user) : NULL;
}

static void api_add_cgi_vars(void *rp)
{
    request_rec *r = rp;
    ap_add_common_vars(r);
    ap_add_cgi_vars(r);
}

static apr_table_t *table_of(request_rec *r, int which)
{
    switch (which) {
    case PHPRS_T_HEADERS_IN: return r->headers_in;
    case PHPRS_T_HEADERS_OUT: return r->headers_out;
    case PHPRS_T_SUBPROCESS_ENV: return r->subprocess_env;
    case PHPRS_T_NOTES: return r->notes;
    case PHPRS_T_ERR_HEADERS_OUT: return r->err_headers_out;
    default: return NULL;
    }
}

static int api_table_count(void *rp, int which)
{
    apr_table_t *t = table_of(rp, which);
    return t ? apr_table_elts(t)->nelts : 0;
}

static void api_table_entry(void *rp, int which, int idx, const char **key, const char **val)
{
    apr_table_t *t = table_of(rp, which);
    const apr_array_header_t *arr;
    const apr_table_entry_t *elts;

    *key = NULL;
    *val = NULL;
    if (!t) {
        return;
    }
    arr = apr_table_elts(t);
    if (idx < 0 || idx >= arr->nelts) {
        return;
    }
    elts = (const apr_table_entry_t *)arr->elts;
    *key = elts[idx].key;
    *val = elts[idx].val;
}

static const char *api_table_get(void *rp, int which, const char *key)
{
    apr_table_t *t = table_of(rp, which);
    return t ? apr_table_get(t, key) : NULL;
}

static void api_table_op(void *rp, int which, int op, const char *key, const char *val)
{
    apr_table_t *t = table_of(rp, which);
    if (!t) {
        return;
    }
    switch (op) {
    case PHPRS_OP_SET: apr_table_set(t, key, val); break;
    case PHPRS_OP_ADD: apr_table_add(t, key, val); break;
    case PHPRS_OP_UNSET: apr_table_unset(t, key); break;
    case PHPRS_OP_CLEAR: apr_table_clear(t); break;
    default: break;
    }
}

/* php_apache_sapi_send_headers(): status, status line, protocol. */
static void api_set_status(void *rp, int status, const char *status_line, int proto_num)
{
    request_rec *r = rp;
    r->status = status;
    if (status_line) {
        r->status_line = apr_pstrdup(r->pool, status_line);
    }
    if (proto_num > 0) {
        r->proto_num = proto_num;
        if (proto_num == 1000) {
            apr_table_set(r->subprocess_env, "force-response-1.0", "true");
        }
    }
}

static void api_set_content_type(void *rp, const char *ct)
{
    request_rec *r = rp;
    ap_set_content_type(r, apr_pstrdup(r->pool, ct));
}

static void api_set_content_length(void *rp, long long len)
{
    ap_set_content_length((request_rec *)rp, (apr_off_t)len);
}

static int api_write(void *rp, const char *buf, size_t len)
{
    request_rec *r = rp;
    while (len > 0) {
        int chunk = len > (size_t)INT_MAX ? INT_MAX : (int)len;
        if (ap_rwrite(buf, chunk, r) < 0) {
            return -1;
        }
        buf += chunk;
        len -= (size_t)chunk;
    }
    return 0;
}

static int api_flush(void *rp)
{
    request_rec *r = rp;
    if (ap_rflush(r) < 0 || r->connection->aborted) {
        return -1;
    }
    return 0;
}

/* Returns -status on error, 0 when there is no body, 1 when there is one. */
static int api_body_setup(void *rp)
{
    request_rec *r = rp;
    int rc = ap_setup_client_block(r, REQUEST_CHUNKED_DECHUNK);
    if (rc != OK) {
        return -rc;
    }
    return ap_should_client_block(r) ? 1 : 0;
}

static long api_body_read(void *rp, char *buf, size_t len)
{
    apr_size_t n = len > (size_t)LONG_MAX ? (apr_size_t)LONG_MAX : (apr_size_t)len;
    return ap_get_client_block((request_rec *)rp, buf, n);
}

static void api_log_request(void *rp, int level, const char *msg)
{
    ap_log_rerror(APLOG_MARK, level, 0, (request_rec *)rp, "%s", msg);
}

static void api_log_server(int level, const char *msg)
{
    ap_log_error(APLOG_MARK, level, 0, NULL, "%s", msg);
}

static const char *api_server_banner(void)
{
    return ap_get_server_banner();
}

static int api_module_count(void)
{
    int n = 0;
    while (ap_loaded_modules[n]) {
        n++;
    }
    return n;
}

static const char *api_module_name(int idx)
{
    return ap_loaded_modules[idx]->name;
}

static int api_dir_count(void *rp)
{
    request_rec *r = rp;
    phprs_dir_conf *c = ap_get_module_config(r->per_dir_config, &php_module);
    return c ? c->entries->nelts : 0;
}

static void api_dir_entry(void *rp, int idx, const char **name, const char **value, int *status, int *htaccess)
{
    request_rec *r = rp;
    phprs_dir_conf *c = ap_get_module_config(r->per_dir_config, &php_module);
    phprs_dir_entry *e = (phprs_dir_entry *)c->entries->elts;
    *name = e[idx].name;
    *value = e[idx].value;
    *status = e[idx].status;
    *htaccess = e[idx].htaccess;
}

static void *api_sub_lookup(void *rp, const char *uri)
{
    request_rec *r = rp;
    return ap_sub_req_lookup_uri(uri, r, r->output_filters);
}

static int api_sub_run(void *rr)
{
    request_rec *sub = rr;
    /* Work around https://bz.apache.org/bugzilla/show_bug.cgi?id=17629 */
    ap_rflush(sub->main);
    return ap_run_sub_req(sub);
}

static void api_sub_destroy(void *rr)
{
    ap_destroy_sub_req((request_rec *)rr);
}

static void api_set_last_modified(void *rp)
{
    request_rec *r = rp;
    ap_update_mtime(r, r->finfo.mtime);
    ap_set_last_modified(r);
}

static void *api_top_request(void *rp)
{
    request_rec *r = rp;
    while (r->prev) {
        r = r->prev;
    }
    return r;
}

static void *api_main_request(void *rp)
{
    return ((request_rec *)rp)->main;
}

static const phprs_api phprs_api_table = {
    sizeof(phprs_api),
    api_r_str,
    api_r_int,
    api_set_user,
    api_add_cgi_vars,
    api_table_count,
    api_table_entry,
    api_table_get,
    api_table_op,
    api_set_status,
    api_set_content_type,
    api_set_content_length,
    api_write,
    api_flush,
    api_body_setup,
    api_body_read,
    api_log_request,
    api_log_server,
    api_server_banner,
    api_module_count,
    api_module_name,
    api_dir_count,
    api_dir_entry,
    api_sub_lookup,
    api_sub_run,
    api_sub_destroy,
    api_set_last_modified,
    api_top_request,
    api_main_request,
};

/* ---- hooks ---- */

static int php_pre_config(apr_pool_t *pconf, apr_pool_t *plog, apr_pool_t *ptemp)
{
    (void)pconf;
    (void)plog;
    (void)ptemp;
    phprs_ini_dir = NULL;
    phprs_allow_threads = 0;
    return OK;
}

static int php_post_config(apr_pool_t *pconf, apr_pool_t *plog, apr_pool_t *ptemp, server_rec *s)
{
    int threaded = 0;
    const char *version;
    (void)plog;
    (void)ptemp;

    if (ap_mpm_query(AP_MPMQ_IS_THREADED, &threaded) != APR_SUCCESS) {
        threaded = 0;
    }
    if (threaded != AP_MPMQ_NOT_SUPPORTED && !phprs_allow_threads) {
        ap_log_error(APLOG_MARK, APLOG_CRIT, 0, s,
                     "Apache is running a threaded MPM, but the phprust PHP module is not thread-safe: "
                     "use mpm_prefork, or set \"PhpRustAllowThreads On\" to serialise PHP requests "
                     "within each process (one PHP request at a time per child)");
        return DONE;
    }
    if (phprust_post_config(phprs_ini_dir) != 0) {
        return DONE;
    }
    version = phprust_version_component();
    if (version) {
        ap_add_version_component(pconf, version);
    }
    return OK;
}

static void php_child_init(apr_pool_t *pchild, server_rec *s)
{
    (void)pchild;
    (void)s;
    phprust_child_init();
}

static int php_handler(request_rec *r)
{
    if (!r->handler) {
        return DECLINED;
    }
    return phprust_handler(r);
}

static void php_register_hooks(apr_pool_t *p)
{
    (void)p;
    phprust_set_api(&phprs_api_table);
    ap_hook_pre_config(php_pre_config, NULL, NULL, APR_HOOK_MIDDLE);
    ap_hook_post_config(php_post_config, NULL, NULL, APR_HOOK_MIDDLE);
    ap_hook_child_init(php_child_init, NULL, NULL, APR_HOOK_MIDDLE);
    ap_hook_handler(php_handler, NULL, NULL, APR_HOOK_MIDDLE);
}

static const module php_module_template = {
    MODULE_MAGIC_NUMBER_MAJOR,
    MODULE_MAGIC_NUMBER_MINOR,
    -1,
    "mod_php.c",
    NULL,
    NULL,
    MODULE_MAGIC_COOKIE,
    NULL,
    create_php_config,   /* create per-directory config structure */
    merge_php_config,    /* merge per-directory config structures */
    NULL,                /* create per-server config structure */
    NULL,                /* merge per-server config structures */
    php_dir_cmds,        /* command table */
    php_register_hooks,  /* register hooks */
#ifdef AP_MODULE_FLAG_NONE
    AP_MODULE_FLAG_NONE
#endif
};

__attribute__((constructor)) static void phprs_install_module(void)
{
    memcpy(&php_module, &php_module_template, sizeof(module));
}
