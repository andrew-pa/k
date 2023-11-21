#![no_std]
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct TestMessage {}

/// A [Message] that can be sent to the kernel.
#[derive(Serialize, Deserialize)]
pub enum Message {
    Test(TestMessage),
}
