// Re-export from cdn-log crate.
// The LogEntry struct, push_log_entry, and init_log_queue are now in cdn-log.
// This module is kept for backward compatibility with existing imports.
pub use cdn_log::entry::LogEntry;
pub use cdn_log::queue::{init_log_queue, push_log_entry};
