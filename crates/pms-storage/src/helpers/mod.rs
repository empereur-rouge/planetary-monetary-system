mod encoding;
mod time_index;
mod activity_keys;
mod classify;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

// Re-export everything to maintain the existing public API.
// All callers use `crate::helpers::X` or `pms_storage::helpers::X`.

pub use encoding::{
    be_u64,
    from_be_i64,
    now_ms,
    ts_to_be,
    be_to_ts,
    now_ms_i64,
    u64_to_le,
    le_to_u64,
    ts_bytes,
    be_to_i64,
};

pub use time_index::{
    key_time_index,
    parse_time_index_key,
};

pub use activity_keys::{
    key_addr_activity,
    prefix_addr_activity,
    parse_addr_activity_key,
    key_addr_type_activity,
    prefix_addr_type_activity,
    parse_addr_type_activity_key,
};

pub use classify::{
    extract_involved_addresses,
    ActivityCategory,
    extract_involved_with_category,
    is_fee_output_only,
    classify_for_storage,
    precompute_all_items,
};
