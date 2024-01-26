#[repr(u16)]
pub enum SystemCallNumber {
    Exit = 1,
    WriteLog = 4,
    Yield = 10,
    WaitForMessage = 11,
}
