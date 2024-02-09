//! Device drivers for hardware busses (ex. PCIe).
//!
//! Bus modules should provide basic data structures for interacting with devices on the bus, as
//! well as bus initialization and device discovery.
pub mod pcie;
