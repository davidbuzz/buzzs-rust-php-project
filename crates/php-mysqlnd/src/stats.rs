//! The mysqlnd statistics counters behind `mysqli_get_client_stats()` and
//! `mysqli_get_connection_stats()` (ref: `mysqlnd_statistics.c`).
//!
//! Counters are unsigned 64-bit and wrap like the C `uint64_t` arithmetic
//! (`active_connections` can therefore "underflow").

use std::cell::RefCell;

use crate::ini;

/// The collected statistics (`enum mysqlnd_collected_stats`), in order.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Stat {
    BytesSent,
    BytesReceived,
    PacketsSent,
    PacketsReceived,
    ProtocolOverheadIn,
    ProtocolOverheadOut,
    BytesReceivedOk,
    BytesReceivedEof,
    BytesReceivedRsetHeader,
    BytesReceivedRsetFieldMeta,
    BytesReceivedRsetRow,
    BytesReceivedPrepareResponse,
    BytesReceivedChangeUser,
    PacketsSentCmd,
    PacketsReceivedOk,
    PacketsReceivedEof,
    PacketsReceivedRsetHeader,
    PacketsReceivedRsetFieldMeta,
    PacketsReceivedRsetRow,
    PacketsReceivedPrepareResponse,
    PacketsReceivedChangeUser,
    RsetQuery,
    NonRsetQuery,
    NoIndexUsed,
    BadIndexUsed,
    QueryWasSlow,
    BufferedSets,
    UnbufferedSets,
    PsBufferedSets,
    PsUnbufferedSets,
    FlushedNormalSets,
    FlushedPsSets,
    PsPreparedNeverExecuted,
    PsPreparedOnceUsed,
    RowsFetchedFromServerNormal,
    RowsFetchedFromServerPs,
    RowsBufferedFromClientNormal,
    RowsBufferedFromClientPs,
    RowsFetchedFromClientNormalBuf,
    RowsFetchedFromClientNormalUnbuf,
    RowsFetchedFromClientPsBuf,
    RowsFetchedFromClientPsUnbuf,
    RowsFetchedFromClientPsCursor,
    RowsAffectedNormal,
    RowsAffectedPs,
    RowsSkippedNormal,
    RowsSkippedPs,
    CopyOnWriteSaved,
    CopyOnWritePerformed,
    CmdBufferTooSmall,
    ConnectSuccess,
    ConnectFailure,
    ConnectReused,
    Reconnect,
    PconnectSuccess,
    OpenedConnections,
    OpenedPersistentConnections,
    CloseExplicit,
    CloseImplicit,
    CloseDisconnect,
    CloseInMiddle,
    FreeResultExplicit,
    FreeResultImplicit,
    StmtCloseExplicit,
    StmtCloseImplicit,
    MemEmallocCount,
    MemEmallocAmount,
    MemEcallocCount,
    MemEcallocAmount,
    MemEreallocCount,
    MemEreallocAmount,
    MemEfreeCount,
    MemEfreeAmount,
    MemMallocCount,
    MemMallocAmount,
    MemCallocCount,
    MemCallocAmount,
    MemReallocCount,
    MemReallocAmount,
    MemFreeCount,
    MemFreeAmount,
    MemEstrndupCount,
    MemStrndupCount,
    MemEstrdupCount,
    MemStrdupCount,
    MemEdupCount,
    MemDupCount,
    TextTypeFetchedNull,
    TextTypeFetchedBit,
    TextTypeFetchedInt8,
    TextTypeFetchedInt16,
    TextTypeFetchedInt24,
    TextTypeFetchedInt32,
    TextTypeFetchedInt64,
    TextTypeFetchedDecimal,
    TextTypeFetchedFloat,
    TextTypeFetchedDouble,
    TextTypeFetchedDate,
    TextTypeFetchedYear,
    TextTypeFetchedTime,
    TextTypeFetchedDatetime,
    TextTypeFetchedTimestamp,
    TextTypeFetchedString,
    TextTypeFetchedJson,
    TextTypeFetchedBlob,
    TextTypeFetchedEnum,
    TextTypeFetchedSet,
    TextTypeFetchedGeometry,
    TextTypeFetchedOther,
    BinaryTypeFetchedNull,
    BinaryTypeFetchedBit,
    BinaryTypeFetchedInt8,
    BinaryTypeFetchedInt16,
    BinaryTypeFetchedInt24,
    BinaryTypeFetchedInt32,
    BinaryTypeFetchedInt64,
    BinaryTypeFetchedDecimal,
    BinaryTypeFetchedFloat,
    BinaryTypeFetchedDouble,
    BinaryTypeFetchedDate,
    BinaryTypeFetchedYear,
    BinaryTypeFetchedTime,
    BinaryTypeFetchedDatetime,
    BinaryTypeFetchedTimestamp,
    BinaryTypeFetchedString,
    BinaryTypeFetchedBlob,
    BinaryTypeFetchedEnum,
    BinaryTypeFetchedSet,
    BinaryTypeFetchedGeometry,
    BinaryTypeFetchedOther,
    InitCommandExecutedCount,
    InitCommandFailedCount,
    ComQuit,
    ComInitDb,
    ComQuery,
    ComFieldList,
    ComCreateDb,
    ComDropDb,
    ComRefresh,
    ComShutdown,
    ComStatistics,
    ComProcessInfo,
    ComConnect,
    ComProcessKill,
    ComDebug,
    ComPing,
    ComTime,
    ComDelayedInsert,
    ComChangeUser,
    ComBinlogDump,
    ComTableDump,
    ComConnectOut,
    ComRegisterSlave,
    ComStmtPrepare,
    ComStmtExecute,
    ComStmtSendLongData,
    ComStmtClose,
    ComStmtReset,
    ComSetOption,
    ComStmtFetch,
    ComDaemon,
    BytesReceivedPureDataText,
    BytesReceivedPureDataPs,
}

/// Number of statistics (`STAT_LAST`).
pub const STAT_COUNT: usize = 163;

/// Names reported by `mysqli_get_client_stats()` (`mysqlnd_stats_values_names`),
/// indexed by [`Stat`].
pub static STAT_NAMES: [&str; STAT_COUNT] = [
    "bytes_sent",
    "bytes_received",
    "packets_sent",
    "packets_received",
    "protocol_overhead_in",
    "protocol_overhead_out",
    "bytes_received_ok_packet",
    "bytes_received_eof_packet",
    "bytes_received_rset_header_packet",
    "bytes_received_rset_field_meta_packet",
    "bytes_received_rset_row_packet",
    "bytes_received_prepare_response_packet",
    "bytes_received_change_user_packet",
    "packets_sent_command",
    "packets_received_ok",
    "packets_received_eof",
    "packets_received_rset_header",
    "packets_received_rset_field_meta",
    "packets_received_rset_row",
    "packets_received_prepare_response",
    "packets_received_change_user",
    "result_set_queries",
    "non_result_set_queries",
    "no_index_used",
    "bad_index_used",
    "slow_queries",
    "buffered_sets",
    "unbuffered_sets",
    "ps_buffered_sets",
    "ps_unbuffered_sets",
    "flushed_normal_sets",
    "flushed_ps_sets",
    "ps_prepared_never_executed",
    "ps_prepared_once_executed",
    "rows_fetched_from_server_normal",
    "rows_fetched_from_server_ps",
    "rows_buffered_from_client_normal",
    "rows_buffered_from_client_ps",
    "rows_fetched_from_client_normal_buffered",
    "rows_fetched_from_client_normal_unbuffered",
    "rows_fetched_from_client_ps_buffered",
    "rows_fetched_from_client_ps_unbuffered",
    "rows_fetched_from_client_ps_cursor",
    "rows_affected_normal",
    "rows_affected_ps",
    "rows_skipped_normal",
    "rows_skipped_ps",
    "copy_on_write_saved",
    "copy_on_write_performed",
    "command_buffer_too_small",
    "connect_success",
    "connect_failure",
    "connection_reused",
    "reconnect",
    "pconnect_success",
    "active_connections",
    "active_persistent_connections",
    "explicit_close",
    "implicit_close",
    "disconnect_close",
    "in_middle_of_command_close",
    "explicit_free_result",
    "implicit_free_result",
    "explicit_stmt_close",
    "implicit_stmt_close",
    "mem_emalloc_count",
    "mem_emalloc_amount",
    "mem_ecalloc_count",
    "mem_ecalloc_amount",
    "mem_erealloc_count",
    "mem_erealloc_amount",
    "mem_efree_count",
    "mem_efree_amount",
    "mem_malloc_count",
    "mem_malloc_amount",
    "mem_calloc_count",
    "mem_calloc_amount",
    "mem_realloc_count",
    "mem_realloc_amount",
    "mem_free_count",
    "mem_free_amount",
    "mem_estrndup_count",
    "mem_strndup_count",
    "mem_estrdup_count",
    "mem_strdup_count",
    "mem_edupl_count",
    "mem_dupl_count",
    "proto_text_fetched_null",
    "proto_text_fetched_bit",
    "proto_text_fetched_tinyint",
    "proto_text_fetched_short",
    "proto_text_fetched_int24",
    "proto_text_fetched_int",
    "proto_text_fetched_bigint",
    "proto_text_fetched_decimal",
    "proto_text_fetched_float",
    "proto_text_fetched_double",
    "proto_text_fetched_date",
    "proto_text_fetched_year",
    "proto_text_fetched_time",
    "proto_text_fetched_datetime",
    "proto_text_fetched_timestamp",
    "proto_text_fetched_string",
    "proto_text_fetched_blob",
    "proto_text_fetched_enum",
    "proto_text_fetched_set",
    "proto_text_fetched_geometry",
    "proto_text_fetched_other",
    "proto_binary_fetched_null",
    "proto_binary_fetched_bit",
    "proto_binary_fetched_tinyint",
    "proto_binary_fetched_short",
    "proto_binary_fetched_int24",
    "proto_binary_fetched_int",
    "proto_binary_fetched_bigint",
    "proto_binary_fetched_decimal",
    "proto_binary_fetched_float",
    "proto_binary_fetched_double",
    "proto_binary_fetched_date",
    "proto_binary_fetched_year",
    "proto_binary_fetched_time",
    "proto_binary_fetched_datetime",
    "proto_binary_fetched_timestamp",
    "proto_binary_fetched_string",
    "proto_binary_fetched_json",
    "proto_binary_fetched_blob",
    "proto_binary_fetched_enum",
    "proto_binary_fetched_set",
    "proto_binary_fetched_geometry",
    "proto_binary_fetched_other",
    "init_command_executed_count",
    "init_command_failed_count",
    "com_quit",
    "com_init_db",
    "com_query",
    "com_field_list",
    "com_create_db",
    "com_drop_db",
    "com_refresh",
    "com_shutdown",
    "com_statistics",
    "com_process_info",
    "com_connect",
    "com_process_kill",
    "com_debug",
    "com_ping",
    "com_time",
    "com_delayed_insert",
    "com_change_user",
    "com_binlog_dump",
    "com_table_dump",
    "com_connect_out",
    "com_register_slave",
    "com_stmt_prepare",
    "com_stmt_execute",
    "com_stmt_send_long_data",
    "com_stmt_close",
    "com_stmt_reset",
    "com_stmt_set_option",
    "com_stmt_fetch",
    "com_deamon",
    "bytes_received_real_data_normal",
    "bytes_received_real_data_ps",
];

/// One set of counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    values: [u64; STAT_COUNT],
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            values: [0; STAT_COUNT],
        }
    }
}

impl Stats {
    /// Current value of a counter.
    #[must_use]
    pub fn get(&self, s: Stat) -> u64 {
        self.values[s as usize]
    }

    /// Adds `v` (wrapping) to a counter.
    pub fn add(&mut self, s: Stat, v: u64) {
        let slot = &mut self.values[s as usize];
        *slot = slot.wrapping_add(v);
    }

    /// Subtracts one (wrapping) from a counter.
    pub fn dec(&mut self, s: Stat) {
        let slot = &mut self.values[s as usize];
        *slot = slot.wrapping_sub(1);
    }

    /// `(name, value-as-decimal-string)` pairs in the exact order of
    /// `mysqli_get_client_stats()`; values are strings in PHP.
    #[must_use]
    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        STAT_NAMES
            .iter()
            .zip(self.values.iter())
            .map(|(n, v)| (*n, v.to_string()))
            .collect()
    }
}

thread_local! {
    static GLOBAL: RefCell<Stats> = RefCell::new(Stats::default());
}

/// Snapshot of the process-wide counters (`mysqli_get_client_stats`).
#[must_use]
pub fn global() -> Stats {
    GLOBAL.with(|g| g.borrow().clone())
}

/// Resets the process-wide counters (test helper).
pub fn reset_global() {
    GLOBAL.with(|g| *g.borrow_mut() = Stats::default());
}

/// `MYSQLND_INC_GLOBAL_STATISTIC_W_VALUE`
pub fn inc_global(s: Stat, v: u64) {
    if ini::collect_statistics() {
        GLOBAL.with(|g| g.borrow_mut().add(s, v));
    }
}

/// `MYSQLND_DEC_GLOBAL_STATISTIC`
pub fn dec_global(s: Stat) {
    if ini::collect_statistics() {
        GLOBAL.with(|g| g.borrow_mut().dec(s));
    }
}

/// Like [`inc_conn`] but addressed by raw index (`STAT_COM_QUIT + cmd - 1`
/// style arithmetic); out-of-range indices are ignored.
pub fn inc_conn_index(conn: &mut Stats, idx: usize, v: u64) {
    if idx < STAT_COUNT && ini::collect_statistics() {
        GLOBAL.with(|g| {
            let mut g = g.borrow_mut();
            g.values[idx] = g.values[idx].wrapping_add(v);
        });
        conn.values[idx] = conn.values[idx].wrapping_add(v);
    }
}

/// `MYSQLND_INC_CONN_STATISTIC_W_VALUE`: bumps both the global and the
/// connection counters.
pub fn inc_conn(conn: &mut Stats, s: Stat, v: u64) {
    if ini::collect_statistics() {
        GLOBAL.with(|g| g.borrow_mut().add(s, v));
        conn.add(s, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_line_up() {
        assert_eq!(STAT_NAMES[Stat::BytesSent as usize], "bytes_sent");
        assert_eq!(STAT_NAMES[Stat::ComQuit as usize], "com_quit");
        assert_eq!(
            STAT_NAMES[Stat::OpenedConnections as usize],
            "active_connections"
        );
        assert_eq!(
            STAT_NAMES[Stat::BytesReceivedPureDataPs as usize],
            "bytes_received_real_data_ps"
        );
        assert_eq!(STAT_NAMES.len(), 163);
    }

    #[test]
    fn wrapping_counters() {
        let mut s = Stats::default();
        s.dec(Stat::OpenedConnections);
        assert_eq!(s.get(Stat::OpenedConnections), u64::MAX);
        s.add(Stat::OpenedConnections, 2);
        assert_eq!(s.get(Stat::OpenedConnections), 1);
        let pairs = s.to_pairs();
        assert_eq!(pairs[0], ("bytes_sent", "0".to_string()));
    }
}
