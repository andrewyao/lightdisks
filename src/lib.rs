pub mod scan;
pub mod tree;

pub fn human_size(bytes: u64) -> String {
    bytesize::ByteSize(bytes).display().iec().to_string()
}
