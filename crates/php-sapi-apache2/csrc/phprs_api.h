/*
 * The function table the C shim (mod_phprust.c) hands to the Rust SAPI.
 *
 * Rust never links against httpd/APR symbols directly: every access to a
 * request_rec goes through these accessors, so the Rust library (and its
 * unit tests) build without Apache.  Keep in sync with `src/ffi.rs`
 * (`ApiTable`); `size` guards against mismatches.
 */
#ifndef PHPRS_API_H
#define PHPRS_API_H

#include <stddef.h>

/* String fields of a request_rec (phprs_api.r_str). */
enum phprs_str_field {
    PHPRS_R_HANDLER = 0,
    PHPRS_R_FILENAME = 1,
    PHPRS_R_URI = 2,
    PHPRS_R_ARGS = 3,
    PHPRS_R_METHOD = 4,
    PHPRS_R_PROTOCOL = 5,
    PHPRS_R_USER = 6,
    PHPRS_R_PATH_INFO = 7,
    PHPRS_R_THE_REQUEST = 8,
    PHPRS_R_STATUS_LINE = 9,
    PHPRS_R_CONTENT_TYPE = 10,
    PHPRS_R_UNPARSED_URI = 11,
    PHPRS_R_RANGE = 12
};

/* Integer fields of a request_rec (phprs_api.r_int). */
enum phprs_int_field {
    PHPRS_R_PROTO_NUM = 0,
    PHPRS_R_REQUEST_TIME = 1,   /* microseconds since the epoch */
    PHPRS_R_FILETYPE = 2,       /* 0 none, 1 regular, 2 directory, 3 other */
    PHPRS_R_UEXEC = 3,          /* finfo.protection & APR_UEXECUTE */
    PHPRS_R_PATH_INFO_REJECTED = 4,
    PHPRS_R_IS_MAIN = 5,
    PHPRS_R_STATUS = 6,
    PHPRS_R_MTIME = 7,          /* microseconds */
    PHPRS_R_CLENGTH = 8,
    PHPRS_R_CHUNKED = 9,
    PHPRS_R_NO_CACHE = 10,
    PHPRS_R_NO_LOCAL_COPY = 11,
    PHPRS_R_ALLOWED = 12,
    PHPRS_R_SENT_BODYCT = 13,
    PHPRS_R_BYTES_SENT = 14,
    PHPRS_R_ABORTED = 15,
    PHPRS_R_ENV_DIFFERS = 16,   /* r->main && r->subprocess_env != r->main->subprocess_env */
    PHPRS_R_HEADER_ONLY = 17
};

/* Tables of a request_rec. */
enum phprs_table {
    PHPRS_T_HEADERS_IN = 0,
    PHPRS_T_HEADERS_OUT = 1,
    PHPRS_T_SUBPROCESS_ENV = 2,
    PHPRS_T_NOTES = 3,
    PHPRS_T_ERR_HEADERS_OUT = 4
};

/* Table operations (phprs_api.table_op). */
enum phprs_table_op {
    PHPRS_OP_SET = 0,
    PHPRS_OP_ADD = 1,
    PHPRS_OP_UNSET = 2,
    PHPRS_OP_CLEAR = 3
};

typedef struct phprs_api {
    size_t size;
    const char *(*r_str)(void *r, int field);
    long long (*r_int)(void *r, int field);
    void (*set_user)(void *r, const char *user);
    void (*add_cgi_vars)(void *r);
    int (*table_count)(void *r, int table);
    void (*table_entry)(void *r, int table, int idx, const char **key, const char **val);
    const char *(*table_get)(void *r, int table, const char *key);
    void (*table_op)(void *r, int table, int op, const char *key, const char *val);
    void (*set_status)(void *r, int status, const char *status_line, int proto_num);
    void (*set_content_type)(void *r, const char *content_type);
    void (*set_content_length)(void *r, long long len);
    int (*write)(void *r, const char *buf, size_t len);
    int (*flush)(void *r);
    int (*body_setup)(void *r);
    long (*body_read)(void *r, char *buf, size_t len);
    void (*log_request)(void *r, int level, const char *msg);
    void (*log_server)(int level, const char *msg);
    const char *(*server_banner)(void);
    int (*module_count)(void);
    const char *(*module_name)(int idx);
    int (*dir_count)(void *r);
    void (*dir_entry)(void *r, int idx, const char **name, const char **value, int *status, int *htaccess);
    void *(*sub_lookup)(void *r, const char *uri);
    int (*sub_run)(void *rr);
    void (*sub_destroy)(void *rr);
    void (*set_last_modified)(void *r);
    void *(*top_request)(void *r);
    void *(*main_request)(void *r);
} phprs_api;

#endif
