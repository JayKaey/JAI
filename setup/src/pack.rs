//! The setup's payload: its files one after another (the name's length, the name, the size, the
//! bytes), deflated as one stream. `build.rs` packs, the setup unpacks.

const DAMAGED: &str = "This setup is damaged: download it again.";

pub fn pack(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut raw = Vec::new();
    for (name, data) in files {
        raw.extend_from_slice(&(name.len() as u32).to_le_bytes());
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(&(data.len() as u64).to_le_bytes());
        raw.extend_from_slice(data);
    }
    miniz_oxide::deflate::compress_to_vec(&raw, 6)
}

/// The files, each a plain name (no folder) and its bytes.
pub fn unpack(packed: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let raw = miniz_oxide::inflate::decompress_to_vec(packed).map_err(|_| DAMAGED.to_string())?;
    let mut rest = &raw[..];
    let mut files = Vec::new();
    while !rest.is_empty() {
        let n = u32::from_le_bytes(take(&mut rest, 4)?.try_into().unwrap()) as usize;
        let name = String::from_utf8(take(&mut rest, n)?.to_vec()).map_err(|_| DAMAGED.to_string())?;
        let size = u64::from_le_bytes(take(&mut rest, 8)?.try_into().unwrap());
        let data = take(&mut rest, usize::try_from(size).map_err(|_| DAMAGED.to_string())?)?.to_vec();
        let plain = !name.is_empty() && !name.contains(['/', '\\', ':']) && name != "." && name != "..";
        if !plain {
            return Err(DAMAGED.into());
        }
        files.push((name, data));
    }
    Ok(files)
}

fn take<'a>(rest: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    if rest.len() < n {
        return Err(DAMAGED.into());
    }
    let (head, tail) = rest.split_at(n);
    *rest = tail;
    Ok(head)
}
