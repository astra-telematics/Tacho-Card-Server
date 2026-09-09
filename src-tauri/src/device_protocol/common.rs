pub struct ProtocolError {
    pub message: String,
}
pub type Result<T> = std::result::Result<T, ProtocolError>;

