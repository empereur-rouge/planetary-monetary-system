#[derive(Debug)]
pub enum PutResult {
    Inserted,
    AlreadyExists,
    Rejected(String),
}
