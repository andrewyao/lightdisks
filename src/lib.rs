pub mod app;
pub mod color;
pub mod hidden;
pub mod layout;
pub mod render;
pub mod scan;
pub mod tree;

pub fn human_size(bytes: u64) -> String {
    bytesize::ByteSize(bytes).display().iec().to_string()
}
