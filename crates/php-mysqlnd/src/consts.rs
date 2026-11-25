// Derived from php-src ext/mysqlnd/mysqlnd_enum_n_def.h. Similarity metric to original: 48%
//   Copyright © The PHP Group and Contributors.
//   Authors: Andrey Hristov <andrey@php.net>
//            Ulf Wendel <uw@php.net>
//            Georg Richter <georg@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! Protocol constants: capability/status flags, field types and flags,
//! commands, client error codes and the `MYSQLI_*` constant table
//! (ref: `mysqlnd_enum_n_def.h`, `ext/mysqli/mysqli.stub.php`).

/// Maximum payload of one wire packet (`MYSQLND_MAX_PACKET_SIZE`).
pub const MAX_PACKET_SIZE: usize = 256 * 256 * 256 - 1;
/// Largest logical packet mysqlnd will assemble (`MYSQLND_ASSEMBLED_PACKET_MAX_SIZE`).
pub const ASSEMBLED_PACKET_MAX_SIZE: u64 = 3 * 1024 * 1024 * 1024;
/// Length of the 4.1 scramble.
pub const SCRAMBLE_LENGTH: usize = 20;
/// Length of the pre-4.1 scramble part in the greeting.
pub const SCRAMBLE_LENGTH_323: usize = 8;
/// Default authentication plugin.
pub const DEFAULT_AUTH_PROTOCOL: &str = "mysql_native_password";
/// The "no error" SQLSTATE.
pub const SQLSTATE_NULL: &str = "00000";
/// SQLSTATE used for client-side errors.
pub const UNKNOWN_SQLSTATE: &str = "HY000";
/// Maximum user name length sent in the auth packet.
pub const MAX_ALLOWED_USER_LEN: usize = 252;
/// Maximum database name length sent in the auth packet.
pub const MAX_ALLOWED_DB_LEN: usize = 1024;
/// Minimum (and default) `mysqlnd.net_cmd_buffer_size`.
pub const NET_CMD_BUFFER_MIN_SIZE: usize = 4096;
/// Size of an error message buffer (`MYSQLND_ERRMSG_SIZE`); messages are truncated to this - 1.
pub const ERRMSG_SIZE: usize = 512;

// ---------------------------------------------------------------- capabilities

/// New more secure passwords.
pub const CLIENT_LONG_PASSWORD: u32 = 1;
/// Found instead of affected rows.
pub const CLIENT_FOUND_ROWS: u32 = 2;
/// Get all column flags.
pub const CLIENT_LONG_FLAG: u32 = 4;
/// One can specify db on connect.
pub const CLIENT_CONNECT_WITH_DB: u32 = 8;
/// Don't allow database.table.column.
pub const CLIENT_NO_SCHEMA: u32 = 16;
/// Can use the compression protocol.
pub const CLIENT_COMPRESS: u32 = 32;
/// ODBC client (reused by mysqlnd as "don't verify server cert").
pub const CLIENT_ODBC: u32 = 64;
/// Can use LOAD DATA LOCAL.
pub const CLIENT_LOCAL_FILES: u32 = 128;
/// Ignore spaces before '('.
pub const CLIENT_IGNORE_SPACE: u32 = 256;
/// New 4.1 protocol.
pub const CLIENT_PROTOCOL_41: u32 = 512;
/// Interactive client.
pub const CLIENT_INTERACTIVE: u32 = 1024;
/// Switch to SSL after handshake.
pub const CLIENT_SSL: u32 = 2048;
/// Ignore sigpipes.
pub const CLIENT_IGNORE_SIGPIPE: u32 = 4096;
/// Client knows about transactions.
pub const CLIENT_TRANSACTIONS: u32 = 8192;
/// Old flag for the 4.1 protocol.
pub const CLIENT_RESERVED: u32 = 16384;
/// New 4.1 authentication.
pub const CLIENT_SECURE_CONNECTION: u32 = 32768;
/// Enable multi-statement support.
pub const CLIENT_MULTI_STATEMENTS: u32 = 1 << 16;
/// Enable multi-results.
pub const CLIENT_MULTI_RESULTS: u32 = 1 << 17;
/// Multi-results in the PS protocol.
pub const CLIENT_PS_MULTI_RESULTS: u32 = 1 << 18;
/// Plugin authentication.
pub const CLIENT_PLUGIN_AUTH: u32 = 1 << 19;
/// Connection attributes.
pub const CLIENT_CONNECT_ATTRS: u32 = 1 << 20;
/// Length-encoded auth response.
pub const CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA: u32 = 1 << 21;
/// Can handle expired passwords.
pub const CLIENT_CAN_HANDLE_EXPIRED_PASSWORDS: u32 = 1 << 22;
/// Session tracking in OK packets.
pub const CLIENT_SESSION_TRACK: u32 = 1 << 23;
/// OK packets replace EOF packets.
pub const CLIENT_DEPRECATE_EOF: u32 = 1 << 24;
/// mysqlnd extension: don't verify the server certificate.
pub const CLIENT_SSL_DONT_VERIFY_SERVER_CERT: u32 = CLIENT_ODBC;
/// Verify the server certificate.
pub const CLIENT_SSL_VERIFY_SERVER_CERT: u32 = 1 << 30;
/// Remember options.
pub const CLIENT_REMEMBER_OPTIONS: u32 = 1 << 31;

/// Capabilities mysqlnd always requests (`MYSQLND_CAPABILITIES`).
pub const MYSQLND_CAPABILITIES: u32 = CLIENT_LONG_PASSWORD
    | CLIENT_LONG_FLAG
    | CLIENT_TRANSACTIONS
    | CLIENT_PROTOCOL_41
    | CLIENT_SECURE_CONNECTION
    | CLIENT_MULTI_RESULTS
    | CLIENT_LOCAL_FILES
    | CLIENT_PLUGIN_AUTH;

// ---------------------------------------------------------------- server status

/// A transaction is active.
pub const SERVER_STATUS_IN_TRANS: u16 = 1;
/// Autocommit mode.
pub const SERVER_STATUS_AUTOCOMMIT: u16 = 2;
/// More results follow (multi query / CALL).
pub const SERVER_MORE_RESULTS_EXISTS: u16 = 8;
/// No good index was used.
pub const SERVER_QUERY_NO_GOOD_INDEX_USED: u16 = 16;
/// No index was used.
pub const SERVER_QUERY_NO_INDEX_USED: u16 = 32;
/// A read-only cursor was opened.
pub const SERVER_STATUS_CURSOR_EXISTS: u16 = 64;
/// The cursor is exhausted.
pub const SERVER_STATUS_LAST_ROW_SENT: u16 = 128;
/// A database was dropped.
pub const SERVER_STATUS_DB_DROPPED: u16 = 256;
/// `NO_BACKSLASH_ESCAPES` sql mode is active.
pub const SERVER_STATUS_NO_BACKSLASH_ESCAPES: u16 = 512;
/// The query was slow.
pub const SERVER_QUERY_WAS_SLOW: u16 = 2048;
/// The result set holds OUT parameters.
pub const SERVER_PS_OUT_PARAMS: u16 = 4096;
/// Session state changed (session tracking).
pub const SERVER_SESSION_STATE_CHANGED: u16 = 1 << 14;

// ---------------------------------------------------------------- field types

/// `MYSQL_TYPE_DECIMAL`
pub const MYSQL_TYPE_DECIMAL: u8 = 0;
/// `MYSQL_TYPE_TINY`
pub const MYSQL_TYPE_TINY: u8 = 1;
/// `MYSQL_TYPE_SHORT`
pub const MYSQL_TYPE_SHORT: u8 = 2;
/// `MYSQL_TYPE_LONG`
pub const MYSQL_TYPE_LONG: u8 = 3;
/// `MYSQL_TYPE_FLOAT`
pub const MYSQL_TYPE_FLOAT: u8 = 4;
/// `MYSQL_TYPE_DOUBLE`
pub const MYSQL_TYPE_DOUBLE: u8 = 5;
/// `MYSQL_TYPE_NULL`
pub const MYSQL_TYPE_NULL: u8 = 6;
/// `MYSQL_TYPE_TIMESTAMP`
pub const MYSQL_TYPE_TIMESTAMP: u8 = 7;
/// `MYSQL_TYPE_LONGLONG`
pub const MYSQL_TYPE_LONGLONG: u8 = 8;
/// `MYSQL_TYPE_INT24`
pub const MYSQL_TYPE_INT24: u8 = 9;
/// `MYSQL_TYPE_DATE`
pub const MYSQL_TYPE_DATE: u8 = 10;
/// `MYSQL_TYPE_TIME`
pub const MYSQL_TYPE_TIME: u8 = 11;
/// `MYSQL_TYPE_DATETIME`
pub const MYSQL_TYPE_DATETIME: u8 = 12;
/// `MYSQL_TYPE_YEAR`
pub const MYSQL_TYPE_YEAR: u8 = 13;
/// `MYSQL_TYPE_NEWDATE`
pub const MYSQL_TYPE_NEWDATE: u8 = 14;
/// `MYSQL_TYPE_VARCHAR`
pub const MYSQL_TYPE_VARCHAR: u8 = 15;
/// `MYSQL_TYPE_BIT`
pub const MYSQL_TYPE_BIT: u8 = 16;
/// `MYSQL_TYPE_VECTOR`
pub const MYSQL_TYPE_VECTOR: u8 = 242;
/// `MYSQL_TYPE_JSON`
pub const MYSQL_TYPE_JSON: u8 = 245;
/// `MYSQL_TYPE_NEWDECIMAL`
pub const MYSQL_TYPE_NEWDECIMAL: u8 = 246;
/// `MYSQL_TYPE_ENUM`
pub const MYSQL_TYPE_ENUM: u8 = 247;
/// `MYSQL_TYPE_SET`
pub const MYSQL_TYPE_SET: u8 = 248;
/// `MYSQL_TYPE_TINY_BLOB`
pub const MYSQL_TYPE_TINY_BLOB: u8 = 249;
/// `MYSQL_TYPE_MEDIUM_BLOB`
pub const MYSQL_TYPE_MEDIUM_BLOB: u8 = 250;
/// `MYSQL_TYPE_LONG_BLOB`
pub const MYSQL_TYPE_LONG_BLOB: u8 = 251;
/// `MYSQL_TYPE_BLOB`
pub const MYSQL_TYPE_BLOB: u8 = 252;
/// `MYSQL_TYPE_VAR_STRING`
pub const MYSQL_TYPE_VAR_STRING: u8 = 253;
/// `MYSQL_TYPE_STRING`
pub const MYSQL_TYPE_STRING: u8 = 254;
/// `MYSQL_TYPE_GEOMETRY`
pub const MYSQL_TYPE_GEOMETRY: u8 = 255;

/// Whether mysqlnd knows how to decode values of type `t`
/// (`mysqlnd_ps_fetch_functions[t].func != NULL`).
#[must_use]
pub fn is_known_type(t: u8) -> bool {
    matches!(t, 0..=16 | 242 | 245..=255)
}

// ---------------------------------------------------------------- field flags

/// Field can't be NULL.
pub const NOT_NULL_FLAG: u32 = 1;
/// Field is part of a primary key.
pub const PRI_KEY_FLAG: u32 = 2;
/// Field is part of a unique key.
pub const UNIQUE_KEY_FLAG: u32 = 4;
/// Field is part of a key.
pub const MULTIPLE_KEY_FLAG: u32 = 8;
/// Field is a blob.
pub const BLOB_FLAG: u32 = 16;
/// Field is unsigned.
pub const UNSIGNED_FLAG: u32 = 32;
/// Field is zerofill.
pub const ZEROFILL_FLAG: u32 = 64;
/// Field is binary.
pub const BINARY_FLAG: u32 = 128;
/// Field is an enum.
pub const ENUM_FLAG: u32 = 256;
/// Field is auto-increment.
pub const AUTO_INCREMENT_FLAG: u32 = 512;
/// Field is a timestamp.
pub const TIMESTAMP_FLAG: u32 = 1024;
/// Field is a set.
pub const SET_FLAG: u32 = 2048;
/// Field has no default value.
pub const NO_DEFAULT_VALUE_FLAG: u32 = 4096;
/// Field is set to NOW on UPDATE.
pub const ON_UPDATE_NOW_FLAG: u32 = 8192;
/// Field is part of some key.
pub const PART_KEY_FLAG: u32 = 16384;
/// Field is grouped.
pub const GROUP_FLAG: u32 = 32768;
/// Field is numeric.
pub const NUM_FLAG: u32 = 32768;

// ---------------------------------------------------------------- commands

/// Client/server commands (`enum php_mysqlnd_server_command`).
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    Sleep = 0,
    Quit = 1,
    InitDb = 2,
    Query = 3,
    FieldList = 4,
    CreateDb = 5,
    DropDb = 6,
    Refresh = 7,
    Shutdown = 8,
    Statistics = 9,
    ProcessInfo = 10,
    Connect = 11,
    ProcessKill = 12,
    Debug = 13,
    Ping = 14,
    Time = 15,
    DelayedInsert = 16,
    ChangeUser = 17,
    BinlogDump = 18,
    TableDump = 19,
    ConnectOut = 20,
    RegisterSlave = 21,
    StmtPrepare = 22,
    StmtExecute = 23,
    StmtSendLongData = 24,
    StmtClose = 25,
    StmtReset = 26,
    SetOption = 27,
    StmtFetch = 28,
    Daemon = 29,
    BinlogDumpGtid = 30,
    ResetConnection = 31,
}

impl Command {
    /// The name used in mysqlnd diagnostics (`mysqlnd_command_to_text`).
    #[must_use]
    pub fn as_text(self) -> &'static str {
        const NAMES: [&str; 32] = [
            "SLEEP",
            "QUIT",
            "INIT_DB",
            "QUERY",
            "FIELD_LIST",
            "CREATE_DB",
            "DROP_DB",
            "REFRESH",
            "SHUTDOWN",
            "STATISTICS",
            "PROCESS_INFO",
            "CONNECT",
            "PROCESS_KILL",
            "DEBUG",
            "PING",
            "TIME",
            "DELAYED_INSERT",
            "CHANGE_USER",
            "BINLOG_DUMP",
            "TABLE_DUMP",
            "CONNECT_OUT",
            "REGISTER_SLAVE",
            "STMT_PREPARE",
            "STMT_EXECUTE",
            "STMT_SEND_LONG_DATA",
            "STMT_CLOSE",
            "STMT_RESET",
            "SET_OPTION",
            "STMT_FETCH",
            "DAEMON",
            "BINLOG_DUMP_GTID",
            "RESET_CONNECTION",
        ];
        NAMES[self as usize]
    }
}

// ---------------------------------------------------------------- client errors

/// Unknown `MySQL` error.
pub const CR_UNKNOWN_ERROR: u32 = 2000;
/// Can't connect.
pub const CR_CONNECTION_ERROR: u32 = 2002;
/// `MySQL` server has gone away.
pub const CR_SERVER_GONE_ERROR: u32 = 2006;
/// Out of memory.
pub const CR_OUT_OF_MEMORY: u32 = 2008;
/// Lost connection to `MySQL` server during query.
pub const CR_SERVER_LOST: u32 = 2013;
/// Commands out of sync.
pub const CR_COMMANDS_OUT_OF_SYNC: u32 = 2014;
/// Can't initialize character set.
pub const CR_CANT_FIND_CHARSET: u32 = 2019;
/// Malformed packet.
pub const CR_MALFORMED_PACKET: u32 = 2027;
/// Statement not prepared.
pub const CR_NO_PREPARE_STMT: u32 = 2030;
/// No data supplied for parameters.
pub const CR_PARAMS_NOT_BOUND: u32 = 2031;
/// Invalid parameter number.
pub const CR_INVALID_PARAMETER_NO: u32 = 2034;
/// Can't send long data for non-string/non-binary data types.
pub const CR_INVALID_BUFFER_USE: u32 = 2035;
/// This feature is not implemented yet.
pub const CR_NOT_IMPLEMENTED: u32 = 2054;
/// LOAD DATA LOCAL INFILE file request rejected.
pub const CR_LOAD_DATA_LOCAL_INFILE_REJECTED: u32 = 2068;
/// Client interaction timeout.
pub const CR_CLIENT_INTERACTION_TIMEOUT: u32 = 4031;
/// mysqlnd: LOAD DATA LOCAL file not found.
pub const MYSQLND_EE_FILENOTFOUND: u32 = 7890;

/// "`MySQL` server has gone away"
pub const MSG_SERVER_GONE: &str = "MySQL server has gone away";
/// "Commands out of sync; you can't run this command now"
pub const MSG_OUT_OF_SYNC: &str = "Commands out of sync; you can't run this command now";
/// "Out of memory"
pub const MSG_OUT_OF_MEMORY: &str = "Out of memory";
/// "Statement not prepared"
pub const MSG_STMT_NOT_PREPARED: &str = "Statement not prepared";
/// Long data sent for a parameter not bound as a blob.
pub const MSG_NOT_BOUND_AS_BLOB: &str = "Can't send long data for non-string/non-binary data types";
/// Old-password authentication requested by the server.
pub const MSG_OLD_PASSWD: &str = "mysqlnd cannot connect to MySQL 4.1+ using the old insecure authentication. \
Please use an administration tool to reset your password with the command SET PASSWORD = PASSWORD('your_existing_password'). This will \
store a new, and more secure, hash value in mysql.user. If this user is used in other scripts executed by PHP 5.2 or earlier you might need to remove the old-passwords \
flag from your my.cnf file";

// ---------------------------------------------------------------- misc enums

/// `MYSQLND_FETCH_ASSOC`
pub const FETCH_ASSOC: u32 = 1;
/// `MYSQLND_FETCH_NUM`
pub const FETCH_NUM: u32 = 2;
/// `MYSQLND_FETCH_BOTH`
pub const FETCH_BOTH: u32 = 3;

/// Transaction start: no option.
pub const TRANS_START_NO_OPT: u32 = 0;
/// `WITH CONSISTENT SNAPSHOT`
pub const TRANS_START_WITH_CONSISTENT_SNAPSHOT: u32 = 1;
/// `READ WRITE`
pub const TRANS_START_READ_WRITE: u32 = 2;
/// `READ ONLY`
pub const TRANS_START_READ_ONLY: u32 = 4;
/// Commit/rollback: no option.
pub const TRANS_COR_NO_OPT: u32 = 0;
/// `AND CHAIN`
pub const TRANS_COR_AND_CHAIN: u32 = 1;
/// `AND NO CHAIN`
pub const TRANS_COR_AND_NO_CHAIN: u32 = 2;
/// `RELEASE`
pub const TRANS_COR_RELEASE: u32 = 4;
/// `NO RELEASE`
pub const TRANS_COR_NO_RELEASE: u32 = 8;

/// Refresh grant tables.
pub const REFRESH_GRANT: u32 = 1;
/// Start on new log file.
pub const REFRESH_LOG: u32 = 2;
/// Close all tables.
pub const REFRESH_TABLES: u32 = 4;
/// Flush host cache.
pub const REFRESH_HOSTS: u32 = 8;
/// Flush status variables.
pub const REFRESH_STATUS: u32 = 16;
/// Flush thread cache.
pub const REFRESH_THREADS: u32 = 32;
/// Reset replica info.
pub const REFRESH_REPLICA: u32 = 64;
/// Remove all binary logs.
pub const REFRESH_MASTER: u32 = 128;
/// Backup log.
pub const REFRESH_BACKUP_LOG: u32 = 0x0020_0000;

/// `MYSQL_OPTION_MULTI_STATEMENTS_ON`
pub const OPTION_MULTI_STATEMENTS_ON: u16 = 0;
/// `MYSQL_OPTION_MULTI_STATEMENTS_OFF`
pub const OPTION_MULTI_STATEMENTS_OFF: u16 = 1;

/// Statement attribute: update `max_length`.
pub const STMT_ATTR_UPDATE_MAX_LENGTH: u32 = 0;
/// Statement attribute: cursor type.
pub const STMT_ATTR_CURSOR_TYPE: u32 = 1;
/// Statement attribute: prefetch rows (not supported by mysqlnd).
pub const STMT_ATTR_PREFETCH_ROWS: u32 = 2;
/// No cursor.
pub const CURSOR_TYPE_NO_CURSOR: u64 = 0;
/// Read-only cursor.
pub const CURSOR_TYPE_READ_ONLY: u64 = 1;

/// `MYSQL_NO_DATA` (`mysqli_stmt_fetch` end marker)
pub const MYSQL_NO_DATA: u32 = 100;
/// `MYSQL_DATA_TRUNCATED`
pub const MYSQL_DATA_TRUNCATED: u32 = 101;

/// Client option identifiers (`enum mysqlnd_client_option`).
#[allow(missing_docs)]
pub mod opt {
    pub const MYSQL_OPT_CONNECT_TIMEOUT: u32 = 0;
    pub const MYSQL_OPT_COMPRESS: u32 = 1;
    pub const MYSQL_OPT_NAMED_PIPE: u32 = 2;
    pub const MYSQL_INIT_COMMAND: u32 = 3;
    pub const MYSQL_READ_DEFAULT_FILE: u32 = 4;
    pub const MYSQL_READ_DEFAULT_GROUP: u32 = 5;
    pub const MYSQL_SET_CHARSET_DIR: u32 = 6;
    pub const MYSQL_SET_CHARSET_NAME: u32 = 7;
    pub const MYSQL_OPT_LOCAL_INFILE: u32 = 8;
    pub const MYSQL_OPT_PROTOCOL: u32 = 9;
    pub const MYSQL_SHARED_MEMORY_BASE_NAME: u32 = 10;
    pub const MYSQL_OPT_READ_TIMEOUT: u32 = 11;
    pub const MYSQL_OPT_WRITE_TIMEOUT: u32 = 12;
    pub const MYSQL_OPT_USE_RESULT: u32 = 13;
    pub const MYSQL_OPT_SSL_VERIFY_SERVER_CERT: u32 = 21;
    pub const MYSQL_PLUGIN_DIR: u32 = 22;
    pub const MYSQL_DEFAULT_AUTH: u32 = 23;
    pub const MYSQL_OPT_BIND: u32 = 24;
    pub const MYSQL_OPT_SSL_KEY: u32 = 25;
    pub const MYSQL_OPT_SSL_CERT: u32 = 26;
    pub const MYSQL_OPT_SSL_CA: u32 = 27;
    pub const MYSQL_OPT_SSL_CAPATH: u32 = 28;
    pub const MYSQL_OPT_SSL_CIPHER: u32 = 29;
    pub const MYSQL_OPT_SSL_CRL: u32 = 30;
    pub const MYSQL_OPT_SSL_CRLPATH: u32 = 31;
    pub const MYSQL_OPT_CONNECT_ATTR_RESET: u32 = 32;
    pub const MYSQL_OPT_CONNECT_ATTR_ADD: u32 = 33;
    pub const MYSQL_OPT_CONNECT_ATTR_DELETE: u32 = 34;
    pub const MYSQL_SERVER_PUBLIC_KEY: u32 = 35;
    pub const MYSQL_ENABLE_CLEARTEXT_PLUGIN: u32 = 36;
    pub const MYSQL_OPT_CAN_HANDLE_EXPIRED_PASSWORDS: u32 = 37;
    pub const MYSQL_OPT_SSL_ENFORCE: u32 = 38;
    pub const MYSQL_OPT_MAX_ALLOWED_PACKET: u32 = 39;
    pub const MYSQL_OPT_NET_BUFFER_LENGTH: u32 = 40;
    pub const MYSQL_OPT_TLS_VERSION: u32 = 41;
    pub const MYSQL_OPT_SSL_MODE: u32 = 42;
    pub const MYSQL_OPT_LOAD_DATA_LOCAL_DIR: u32 = 43;
    pub const MYSQLND_OPT_INT_AND_FLOAT_NATIVE: u32 = 201;
    pub const MYSQLND_OPT_NET_CMD_BUFFER_SIZE: u32 = 202;
    pub const MYSQLND_OPT_NET_READ_BUFFER_SIZE: u32 = 203;
    pub const MYSQLND_OPT_SSL_KEY: u32 = 204;
    pub const MYSQLND_OPT_SSL_CERT: u32 = 205;
    pub const MYSQLND_OPT_SSL_CA: u32 = 206;
    pub const MYSQLND_OPT_SSL_CAPATH: u32 = 207;
    pub const MYSQLND_OPT_SSL_CIPHER: u32 = 208;
    pub const MYSQLND_OPT_SSL_PASSPHRASE: u32 = 209;
    pub const MYSQLND_OPT_MAX_ALLOWED_PACKET: u32 = 210;
    pub const MYSQLND_OPT_AUTH_PROTOCOL: u32 = 211;
}

/// Every `MYSQLI_*` integer constant registered by ext/mysqli (built against
/// mysqlnd, 64-bit, release build), in `mysqli.stub.php` order.
pub const MYSQLI_CONSTANTS: &[(&str, i64)] = &[
    (
        "MYSQLI_READ_DEFAULT_GROUP",
        opt::MYSQL_READ_DEFAULT_GROUP as i64,
    ),
    (
        "MYSQLI_READ_DEFAULT_FILE",
        opt::MYSQL_READ_DEFAULT_FILE as i64,
    ),
    (
        "MYSQLI_OPT_CONNECT_TIMEOUT",
        opt::MYSQL_OPT_CONNECT_TIMEOUT as i64,
    ),
    (
        "MYSQLI_OPT_LOCAL_INFILE",
        opt::MYSQL_OPT_LOCAL_INFILE as i64,
    ),
    (
        "MYSQLI_OPT_LOAD_DATA_LOCAL_DIR",
        opt::MYSQL_OPT_LOAD_DATA_LOCAL_DIR as i64,
    ),
    ("MYSQLI_INIT_COMMAND", opt::MYSQL_INIT_COMMAND as i64),
    (
        "MYSQLI_OPT_READ_TIMEOUT",
        opt::MYSQL_OPT_READ_TIMEOUT as i64,
    ),
    (
        "MYSQLI_OPT_NET_CMD_BUFFER_SIZE",
        opt::MYSQLND_OPT_NET_CMD_BUFFER_SIZE as i64,
    ),
    (
        "MYSQLI_OPT_NET_READ_BUFFER_SIZE",
        opt::MYSQLND_OPT_NET_READ_BUFFER_SIZE as i64,
    ),
    (
        "MYSQLI_OPT_INT_AND_FLOAT_NATIVE",
        opt::MYSQLND_OPT_INT_AND_FLOAT_NATIVE as i64,
    ),
    (
        "MYSQLI_OPT_SSL_VERIFY_SERVER_CERT",
        opt::MYSQL_OPT_SSL_VERIFY_SERVER_CERT as i64,
    ),
    ("MYSQLI_OPT_COMPRESS", opt::MYSQL_OPT_COMPRESS as i64),
    (
        "MYSQLI_SERVER_PUBLIC_KEY",
        opt::MYSQL_SERVER_PUBLIC_KEY as i64,
    ),
    ("MYSQLI_CLIENT_SSL", CLIENT_SSL as i64),
    ("MYSQLI_CLIENT_COMPRESS", CLIENT_COMPRESS as i64),
    ("MYSQLI_CLIENT_INTERACTIVE", CLIENT_INTERACTIVE as i64),
    ("MYSQLI_CLIENT_IGNORE_SPACE", CLIENT_IGNORE_SPACE as i64),
    ("MYSQLI_CLIENT_NO_SCHEMA", CLIENT_NO_SCHEMA as i64),
    ("MYSQLI_CLIENT_FOUND_ROWS", CLIENT_FOUND_ROWS as i64),
    (
        "MYSQLI_CLIENT_SSL_VERIFY_SERVER_CERT",
        CLIENT_SSL_VERIFY_SERVER_CERT as i64,
    ),
    (
        "MYSQLI_CLIENT_SSL_DONT_VERIFY_SERVER_CERT",
        CLIENT_SSL_DONT_VERIFY_SERVER_CERT as i64,
    ),
    (
        "MYSQLI_CLIENT_CAN_HANDLE_EXPIRED_PASSWORDS",
        CLIENT_CAN_HANDLE_EXPIRED_PASSWORDS as i64,
    ),
    (
        "MYSQLI_OPT_CAN_HANDLE_EXPIRED_PASSWORDS",
        opt::MYSQL_OPT_CAN_HANDLE_EXPIRED_PASSWORDS as i64,
    ),
    ("MYSQLI_STORE_RESULT", 0),
    ("MYSQLI_USE_RESULT", 1),
    ("MYSQLI_ASYNC", 8),
    ("MYSQLI_STORE_RESULT_COPY_DATA", 16),
    ("MYSQLI_ASSOC", 1),
    ("MYSQLI_NUM", 2),
    ("MYSQLI_BOTH", 3),
    (
        "MYSQLI_STMT_ATTR_UPDATE_MAX_LENGTH",
        STMT_ATTR_UPDATE_MAX_LENGTH as i64,
    ),
    ("MYSQLI_STMT_ATTR_CURSOR_TYPE", STMT_ATTR_CURSOR_TYPE as i64),
    ("MYSQLI_CURSOR_TYPE_NO_CURSOR", 0),
    ("MYSQLI_CURSOR_TYPE_READ_ONLY", 1),
    ("MYSQLI_NOT_NULL_FLAG", NOT_NULL_FLAG as i64),
    ("MYSQLI_PRI_KEY_FLAG", PRI_KEY_FLAG as i64),
    ("MYSQLI_UNIQUE_KEY_FLAG", UNIQUE_KEY_FLAG as i64),
    ("MYSQLI_MULTIPLE_KEY_FLAG", MULTIPLE_KEY_FLAG as i64),
    ("MYSQLI_BLOB_FLAG", BLOB_FLAG as i64),
    ("MYSQLI_UNSIGNED_FLAG", UNSIGNED_FLAG as i64),
    ("MYSQLI_ZEROFILL_FLAG", ZEROFILL_FLAG as i64),
    ("MYSQLI_AUTO_INCREMENT_FLAG", AUTO_INCREMENT_FLAG as i64),
    ("MYSQLI_TIMESTAMP_FLAG", TIMESTAMP_FLAG as i64),
    ("MYSQLI_SET_FLAG", SET_FLAG as i64),
    ("MYSQLI_NUM_FLAG", NUM_FLAG as i64),
    ("MYSQLI_PART_KEY_FLAG", PART_KEY_FLAG as i64),
    ("MYSQLI_GROUP_FLAG", GROUP_FLAG as i64),
    ("MYSQLI_ENUM_FLAG", ENUM_FLAG as i64),
    ("MYSQLI_BINARY_FLAG", BINARY_FLAG as i64),
    ("MYSQLI_NO_DEFAULT_VALUE_FLAG", NO_DEFAULT_VALUE_FLAG as i64),
    ("MYSQLI_ON_UPDATE_NOW_FLAG", ON_UPDATE_NOW_FLAG as i64),
    ("MYSQLI_TYPE_DECIMAL", MYSQL_TYPE_DECIMAL as i64),
    ("MYSQLI_TYPE_TINY", MYSQL_TYPE_TINY as i64),
    ("MYSQLI_TYPE_SHORT", MYSQL_TYPE_SHORT as i64),
    ("MYSQLI_TYPE_LONG", MYSQL_TYPE_LONG as i64),
    ("MYSQLI_TYPE_FLOAT", MYSQL_TYPE_FLOAT as i64),
    ("MYSQLI_TYPE_DOUBLE", MYSQL_TYPE_DOUBLE as i64),
    ("MYSQLI_TYPE_NULL", MYSQL_TYPE_NULL as i64),
    ("MYSQLI_TYPE_TIMESTAMP", MYSQL_TYPE_TIMESTAMP as i64),
    ("MYSQLI_TYPE_LONGLONG", MYSQL_TYPE_LONGLONG as i64),
    ("MYSQLI_TYPE_INT24", MYSQL_TYPE_INT24 as i64),
    ("MYSQLI_TYPE_DATE", MYSQL_TYPE_DATE as i64),
    ("MYSQLI_TYPE_TIME", MYSQL_TYPE_TIME as i64),
    ("MYSQLI_TYPE_DATETIME", MYSQL_TYPE_DATETIME as i64),
    ("MYSQLI_TYPE_YEAR", MYSQL_TYPE_YEAR as i64),
    ("MYSQLI_TYPE_NEWDATE", MYSQL_TYPE_NEWDATE as i64),
    ("MYSQLI_TYPE_ENUM", MYSQL_TYPE_ENUM as i64),
    ("MYSQLI_TYPE_SET", MYSQL_TYPE_SET as i64),
    ("MYSQLI_TYPE_TINY_BLOB", MYSQL_TYPE_TINY_BLOB as i64),
    ("MYSQLI_TYPE_MEDIUM_BLOB", MYSQL_TYPE_MEDIUM_BLOB as i64),
    ("MYSQLI_TYPE_LONG_BLOB", MYSQL_TYPE_LONG_BLOB as i64),
    ("MYSQLI_TYPE_BLOB", MYSQL_TYPE_BLOB as i64),
    ("MYSQLI_TYPE_VAR_STRING", MYSQL_TYPE_VAR_STRING as i64),
    ("MYSQLI_TYPE_STRING", MYSQL_TYPE_STRING as i64),
    ("MYSQLI_TYPE_CHAR", MYSQL_TYPE_TINY as i64),
    ("MYSQLI_TYPE_GEOMETRY", MYSQL_TYPE_GEOMETRY as i64),
    ("MYSQLI_TYPE_VECTOR", MYSQL_TYPE_VECTOR as i64),
    ("MYSQLI_TYPE_JSON", MYSQL_TYPE_JSON as i64),
    ("MYSQLI_TYPE_NEWDECIMAL", MYSQL_TYPE_NEWDECIMAL as i64),
    ("MYSQLI_TYPE_BIT", MYSQL_TYPE_BIT as i64),
    (
        "MYSQLI_SET_CHARSET_NAME",
        opt::MYSQL_SET_CHARSET_NAME as i64,
    ),
    ("MYSQLI_NO_DATA", MYSQL_NO_DATA as i64),
    ("MYSQLI_DATA_TRUNCATED", MYSQL_DATA_TRUNCATED as i64),
    ("MYSQLI_REPORT_INDEX", 4),
    ("MYSQLI_REPORT_ERROR", 1),
    ("MYSQLI_REPORT_STRICT", 2),
    ("MYSQLI_REPORT_ALL", 255),
    ("MYSQLI_REPORT_OFF", 0),
    ("MYSQLI_DEBUG_TRACE_ENABLED", 0),
    (
        "MYSQLI_SERVER_QUERY_NO_GOOD_INDEX_USED",
        SERVER_QUERY_NO_GOOD_INDEX_USED as i64,
    ),
    (
        "MYSQLI_SERVER_QUERY_NO_INDEX_USED",
        SERVER_QUERY_NO_INDEX_USED as i64,
    ),
    ("MYSQLI_SERVER_QUERY_WAS_SLOW", SERVER_QUERY_WAS_SLOW as i64),
    ("MYSQLI_SERVER_PS_OUT_PARAMS", SERVER_PS_OUT_PARAMS as i64),
    ("MYSQLI_REFRESH_GRANT", REFRESH_GRANT as i64),
    ("MYSQLI_REFRESH_LOG", REFRESH_LOG as i64),
    ("MYSQLI_REFRESH_TABLES", REFRESH_TABLES as i64),
    ("MYSQLI_REFRESH_HOSTS", REFRESH_HOSTS as i64),
    ("MYSQLI_REFRESH_STATUS", REFRESH_STATUS as i64),
    ("MYSQLI_REFRESH_THREADS", REFRESH_THREADS as i64),
    ("MYSQLI_REFRESH_REPLICA", REFRESH_REPLICA as i64),
    ("MYSQLI_REFRESH_SLAVE", REFRESH_REPLICA as i64),
    ("MYSQLI_REFRESH_MASTER", REFRESH_MASTER as i64),
    ("MYSQLI_REFRESH_BACKUP_LOG", REFRESH_BACKUP_LOG as i64),
    (
        "MYSQLI_TRANS_START_WITH_CONSISTENT_SNAPSHOT",
        TRANS_START_WITH_CONSISTENT_SNAPSHOT as i64,
    ),
    (
        "MYSQLI_TRANS_START_READ_WRITE",
        TRANS_START_READ_WRITE as i64,
    ),
    ("MYSQLI_TRANS_START_READ_ONLY", TRANS_START_READ_ONLY as i64),
    ("MYSQLI_TRANS_COR_AND_CHAIN", TRANS_COR_AND_CHAIN as i64),
    (
        "MYSQLI_TRANS_COR_AND_NO_CHAIN",
        TRANS_COR_AND_NO_CHAIN as i64,
    ),
    ("MYSQLI_TRANS_COR_RELEASE", TRANS_COR_RELEASE as i64),
    ("MYSQLI_TRANS_COR_NO_RELEASE", TRANS_COR_NO_RELEASE as i64),
];

/// `MYSQLI_IS_MARIADB` (a bool constant; always false for mysqlnd builds).
pub const MYSQLI_IS_MARIADB: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_match_mysqlnd() {
        assert_eq!(MYSQLND_CAPABILITIES, 0x000A_A285);
        assert_eq!(Command::StmtFetch.as_text(), "STMT_FETCH");
        assert_eq!(Command::ResetConnection.as_text(), "RESET_CONNECTION");
    }

    #[test]
    fn known_types() {
        for t in [0u8, 1, 15, 16, 242, 245, 255] {
            assert!(is_known_type(t), "{t}");
        }
        for t in [17u8, 100, 241, 243, 244] {
            assert!(!is_known_type(t), "{t}");
        }
    }

    #[test]
    fn constant_table_has_unique_names() {
        let mut names: Vec<_> = MYSQLI_CONSTANTS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), MYSQLI_CONSTANTS.len());
    }
}
